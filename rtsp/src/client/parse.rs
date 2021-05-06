use bytes::{Buf, Bytes};
use failure::{Error, ResultExt, bail, format_err};
use sdp::{media_description::MediaDescription, session_description::SessionDescription};
use url::Url;
use std::convert::TryFrom;

#[derive(Debug)]
pub struct Presentation {
    pub streams: Vec<Stream>,
    pub base_url: Url,
    pub accept_dynamic_rate: bool,
    sdp: SessionDescription,
}

/// Information about a stream offered within a presentation.
/// Currently if multiple formats are offered, this only describes the first.
#[derive(Debug)]
pub struct Stream {
    /// Media type, as specified in the [IANA SDP parameters media
    /// registry](https://www.iana.org/assignments/sdp-parameters/sdp-parameters.xhtml#sdp-parameters-1).
    pub media: String,

    /// An encoding name, as specified in the [IANA media type
    /// registry](https://www.iana.org/assignments/media-types/media-types.xhtml).
    ///
    /// Commonly used but not specified in that registry: the ONVIF types
    /// claimed in the
    /// [ONVIF Streaming Spec](https://www.onvif.org/specs/stream/ONVIF-Streaming-Spec.pdf):
    /// *   `vnd.onvif.metadata`
    /// *   `vnd.onvif.metadata.gzip`,
    /// *   `vnd.onvif.metadata.exi.onvif`
    /// *   `vnd.onvif.metadata.exi.ext`
    pub encoding_name: String,

    /// RTP payload type.
    /// See the [registry](https://www.iana.org/assignments/rtp-parameters/rtp-parameters.xhtml#rtp-parameters-1).
    /// It's common to use one of the dynamically assigned values, 96–127.
    pub rtp_payload_type: u8,

    /// RTP clock rate, in Hz.
    pub clock_rate: u32,

    /// The metadata, if of a known codec type.
    /// Currently the only supported codec is H.264. This will be extended to
    /// be an enum or something.
    pub metadata: Option<crate::client::video::h264::Metadata>,

    /// The specified control URL, as a raw string.
    /// This can be used via `base_url.join(control)` when creating a `SETUP`
    /// request, or compared directly to the `url` of a `PLAY` response's
    /// `RTP-Info` header.
    pub control: String,

    /// The RTP synchronization source (SSRC), as defined in
    /// [RFC 3550](https://tools.ietf.org/html/rfc3550). This is normally
    /// supplied in the `SETUP` response's `Transport` header. Reolink cameras
    /// instead supply it in the `PLAY` response's `RTP-Info` header.
    pub ssrc: Option<u32>,
}

/// Splits the string on the first occurrence of the specified delimiter and
/// returns prefix before delimiter and suffix after delimiter.
///
/// This matches [str::split_once](https://doc.rust-lang.org/std/primitive.str.html#method.split_once)
/// but doesn't require nightly.
pub(crate) fn split_once(str: &str, delimiter: char) -> Option<(&str, &str)> {
    str.find(delimiter).map(|p| (&str[0..p], &str[p+1..]))
}

impl Stream {
    /// Parses from a [MediaDescription].
    /// On failure, returns an error which is expected to be supplemented with
    /// the [MediaDescription] debug string.
    fn parse(media_description: &MediaDescription) -> Result<Stream, Error> {
        // https://tools.ietf.org/html/rfc8866#section-5.14 says "If the <proto>
        // sub-field is "RTP/AVP" or "RTP/SAVP" the <fmt> sub-fields contain RTP
        // payload type numbers."
        // https://www.iana.org/assignments/sdp-parameters/sdp-parameters.xhtml#sdp-parameters-2
        // shows several other variants, such as "TCP/RTP/AVP". Looking a "RTP" component
        // seems appropriate.
        if !media_description.media_name.protos.iter().any(|p| p == "RTP") {
            bail!("Expected RTP-based proto");
        }

        // RFC 8866 continues: "When a list of payload type numbers is given,
        // this implies that all of these payload formats MAY be used in the
        // session, but the first of these formats SHOULD be used as the default
        // format for the session." Just use the first until we find a stream
        // where this isn't the right thing to do.
        let rtp_payload_type_str = media_description.media_name.formats.first()
            .ok_or_else(|| format_err!("missing RTP payload type"))?;
        let rtp_payload_type = u8::from_str_radix(rtp_payload_type_str, 10)
            .map_err(|_| format_err!("invalid RTP payload type"))?;
        if (rtp_payload_type & 0x80) != 0 {
            bail!("invalid RTP payload type");
        }

        // Capture interesting attributes.
        // RFC 8866: "For dynamic payload type assignments, the "a=rtpmap:"
        // attribute (see Section 6.6) SHOULD be used to map from an RTP payload
        // type number to a media encoding name that identifies the payload
        // format. The "a=fmtp:" attribute MAY be used to specify format
        // parameters (see Section 6.15)."
        let mut rtpmap = None;
        let mut fmtp = None;
        let mut control = None;
        for a in &media_description.attributes {
            if a.key == "rtpmap" {
                let v = a.value.as_ref().ok_or_else(|| format_err!("rtpmap attribute with no value"))?;
                // https://tools.ietf.org/html/rfc8866#section-6.6
                // rtpmap-value = payload-type SP encoding-name
                //   "/" clock-rate [ "/" encoding-params ]
                // payload-type = zero-based-integer
                // encoding-name = token
                // clock-rate = integer
                // encoding-params = channels
                // channels = integer
                let (rtpmap_payload_type, v) = split_once(&v, ' ')
                    .ok_or_else(|| format_err!("invalid rtmap attribute"))?;
                if rtpmap_payload_type == rtp_payload_type_str {
                    rtpmap = Some(v);
                }
            } else if a.key == "fmtp" {
                // Similarly starts with payload-type SP.
                let v = a.value.as_ref().ok_or_else(|| format_err!("rtpmap attribute with no value"))?;
                let (fmtp_payload_type, v) = split_once(&v, ' ')
                    .ok_or_else(|| format_err!("invalid rtmap attribute"))?;
                if fmtp_payload_type == rtp_payload_type_str {
                    fmtp = Some(v);
                }
            } else if a.key == "control" {
                control = Some(a.value.as_ref()
                    .ok_or_else(|| format_err!("control attribute has no value"))?.clone());
            }
        }
        let control = control.ok_or_else(|| format_err!("no control url"))?;

        // TODO: allow statically assigned payload types.
        let rtpmap = rtpmap.ok_or_else(|| format_err!("Expected rtpmap for primary payload type"))?;

        let (encoding_name, rtpmap) = split_once(rtpmap, '/')
            .ok_or_else(|| format_err!("invalid rtpmap attribute"))?;
        let clock_rate_str = match rtpmap.find('/') {
            None => rtpmap,
            Some(i) => &rtpmap[..i],
        };
        let clock_rate = u32::from_str_radix(clock_rate_str, 10)
            .map_err(|_| format_err!("bad clockrate in rtpmap"))?;
        let mut metadata = None;
        
        // https://tools.ietf.org/html/rfc6184#section-8.2.1
        if encoding_name == "H264" {
            if clock_rate != 90000 {
                bail!("H.264 streams must have clock rate of 90000");
            }
            // This isn't an RFC 6184 requirement, but it makes things
            // easier, and I haven't yet encountered a camera which doesn't
            // specify out-of-band parameters.
            let fmtp = fmtp.ok_or_else(|| format_err!(
                "expected out-of-band parameter set for H.264 stream"))?;
            metadata = Some(crate::client::video::h264::Metadata::from_format_specific_params(fmtp)?);
        }

        Ok(Stream {
            media: media_description.media_name.media.clone(),
            encoding_name: encoding_name.to_owned(),
            clock_rate,
            rtp_payload_type,
            metadata,
            control,
            ssrc: None,
        })
    }
}

/// Parses a successful RTSP `DESCRIBE` response into a [Presentation].
pub(crate) fn parse_describe(request_url: Url, response: rtsp_types::Response<Bytes>) -> Result<Presentation, Error> {
    if !matches!(response.header(&rtsp_types::headers::CONTENT_TYPE), Some(v) if v.as_str() == "application/sdp") {
        bail!("Describe response not of expected application/sdp content type: {:#?}", &response);
    }

    let sdp;
    {
        let mut cursor = std::io::Cursor::new(&response.body()[..]);
        sdp = sdp::session_description::SessionDescription::unmarshal(&mut cursor)?;
        if cursor.has_remaining() {
            bail!("garbage after sdp: {:?}",
                  &response.body()[usize::try_from(cursor.position()).unwrap()..]);
        }
    }

    let streams = sdp.media_descriptions
        .iter()
        .enumerate()
        .map(|(i, m)| Stream::parse(&m)
            .with_context(|_| format!("Unable to parse stream {}: {:#?}", i, &m))
            .map_err(Error::from))
        .collect::<Result<Vec<Stream>, Error>>()?;

    let accept_dynamic_rate = matches!(response.header(&crate::X_ACCEPT_DYNAMIC_RATE), Some(h) if h.as_str() == "1");

    // RFC 2326 section C.1.1.
    let base_url = response.header(&rtsp_types::headers::CONTENT_BASE)
        .or_else(|| response.header(&rtsp_types::headers::CONTENT_LOCATION))
        .map(|v| Url::parse(v.as_str()))
        .unwrap_or(Ok(request_url))?;
    
    Ok(Presentation {
        streams,
        accept_dynamic_rate,
        base_url,
        sdp,
    })
}

pub fn parse_setup(
    response: rtsp_types::Response<Bytes>,
    session_id: &mut Option<String>,
    stream: &mut Stream,
) -> Result<(), Error> {
    let response_session = response.header(&rtsp_types::headers::SESSION)
        .ok_or_else(|| format_err!("SETUP response has no Session header"))?;
    let response_session_id = match response_session.as_str().find(';') {
        None => response_session.as_str(),
        Some(i) => &response_session.as_str()[..i],
    };
    match session_id {
      Some(old) if old != response_session_id => {
        bail!("SETUP response changed session id from {:?} to {:?}",
              old, response_session_id);
      },
      Some(_) => {},
      None => *session_id = Some(response_session_id.to_owned()),
    };
    let transport = response.header(&rtsp_types::headers::TRANSPORT)
        .ok_or_else(|| format_err!("SETUP response has no Transport header"))?;
    for part in transport.as_str().split(';') {
        if let Some(ssrc) = part.strip_prefix("ssrc=") {
            let ssrc = u32::from_str_radix(ssrc, 16)
                .map_err(|_| format_err!("Unparseable ssrc {}", ssrc))?;
            stream.ssrc = Some(ssrc);
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use failure::Error;
    use url::Url;

    use crate::client::video::Metadata;

    fn response(raw: &'static [u8]) -> rtsp_types::Response<Bytes> {
        let (msg, len) = rtsp_types::Message::parse(raw).unwrap();
        assert_eq!(len, raw.len());
        match msg {
            rtsp_types::Message::Response(r) => r.map_body(|b| Bytes::from_static(b)),
            _ => panic!("unexpected message type"),
        }
    }

    fn parse_describe(raw_url: &'static str, raw_response: &'static [u8])
        -> Result<super::Presentation, Error> {
        let url = Url::parse(raw_url).unwrap();
        super::parse_describe(url, response(raw_response))
    }

    #[test]
    fn dahua_h264_aac_onvif() {
        let mut p = parse_describe(
            "rtsp://192.168.5.111:554/cam/realmonitor?channel=1&subtype=1&unicast=true&proto=Onvif",
            include_bytes!("testdata/dahua_describe_h264_aac_onvif.txt")).unwrap();
        assert_eq!(
            p.base_url.as_str(),
            "rtsp://192.168.5.111:554/cam/realmonitor?channel=1&subtype=1&unicast=true&proto=Onvif/");
        assert!(p.accept_dynamic_rate);

        assert_eq!(p.streams.len(), 3);

        // H.264 video stream.
        assert_eq!(p.streams[0].control, "trackID=0");
        assert_eq!(p.streams[0].media, "video");
        assert_eq!(p.streams[0].encoding_name, "H264");
        assert_eq!(p.streams[0].rtp_payload_type, 96);
        assert_eq!(p.streams[0].clock_rate, 90_000);
        let metadata = p.streams[0].metadata.as_ref().unwrap();
        assert_eq!(metadata.rfc6381_codec(), "avc1.64001E");
        assert_eq!(metadata.pixel_dimensions(), (704, 480));
        assert_eq!(metadata.pixel_aspect_ratio(), None);
        assert_eq!(metadata.frame_rate(), Some((2, 30)));

        // .mp4 audio stream.
        assert_eq!(p.streams[1].control, "trackID=1");
        assert_eq!(p.streams[1].media, "audio");
        assert_eq!(p.streams[1].encoding_name, "MPEG4-GENERIC");
        assert_eq!(p.streams[1].rtp_payload_type, 97);
        assert_eq!(p.streams[1].clock_rate, 48_000);
        assert!(p.streams[1].metadata.is_none());

        // ONVIF metadata stream.
        assert_eq!(p.streams[2].control, "trackID=4");
        assert_eq!(p.streams[2].media, "application");
        assert_eq!(p.streams[2].encoding_name, "vnd.onvif.metadata");
        assert_eq!(p.streams[2].rtp_payload_type, 107);
        assert_eq!(p.streams[2].clock_rate, 90_000);
        assert!(p.streams[2].metadata.is_none());

        let mut session_id = None;
        super::parse_setup(
            response(include_bytes!("testdata/dahua_setup.txt")),
            &mut session_id,
            &mut p.streams[0]
        ).unwrap();
        assert_eq!(session_id, Some("634214675641".to_owned()));
        assert_eq!(p.streams[0].ssrc, Some(0x30a98ee7));
    }

    #[test]
    fn dahua_h265_pcma() {
        let p = parse_describe(
            "rtsp://192.168.5.111:554/cam/realmonitor?channel=1&subtype=2",
            include_bytes!("testdata/dahua_describe_h265_pcma.txt")).unwrap();

        // Abridged test; similar to the other Dahua test.
        assert_eq!(p.streams.len(), 2);
        assert_eq!(p.streams[0].media, "video");
        assert_eq!(p.streams[0].encoding_name, "H265");
        assert_eq!(p.streams[0].rtp_payload_type, 98);
        assert!(p.streams[1].metadata.is_none());
        assert_eq!(p.streams[1].media, "audio");
        assert_eq!(p.streams[1].encoding_name, "PCMA");
        assert_eq!(p.streams[1].rtp_payload_type, 8);
        assert!(p.streams[1].metadata.is_none());
    }

    #[test]
    fn hikvision() {
        let mut p = parse_describe(
            "rtsp://192.168.5.106:554/Streaming/Channels/101?transportmode=unicast&Profile=Profile_1",
            include_bytes!("testdata/hikvision_describe.txt")).unwrap();
        assert_eq!(
            p.base_url.as_str(),
            "rtsp://192.168.5.106:554/Streaming/Channels/101/");
        assert!(!p.accept_dynamic_rate);

        assert_eq!(p.streams.len(), 2);

        // H.264 video stream.
        assert_eq!(p.streams[0].control, "rtsp://192.168.5.106:554/Streaming/Channels/101/trackID=1?transportmode=unicast&profile=Profile_1");
        assert_eq!(p.streams[0].media, "video");
        assert_eq!(p.streams[0].encoding_name, "H264");
        assert_eq!(p.streams[0].rtp_payload_type, 96);
        assert_eq!(p.streams[0].clock_rate, 90_000);
        let metadata = p.streams[0].metadata.as_ref().unwrap();
        assert_eq!(metadata.rfc6381_codec(), "avc1.4D0029");
        assert_eq!(metadata.pixel_dimensions(), (1920, 1080));
        assert_eq!(metadata.pixel_aspect_ratio(), None);
        assert_eq!(metadata.frame_rate(), Some((2_000, 60_000)));

        // ONVIF metadata stream.
        assert_eq!(p.streams[1].control, "rtsp://192.168.5.106:554/Streaming/Channels/101/trackID=3?transportmode=unicast&profile=Profile_1");
        assert_eq!(p.streams[1].media, "application");
        assert_eq!(p.streams[1].encoding_name, "vnd.onvif.metadata");
        assert_eq!(p.streams[1].rtp_payload_type, 107);
        assert_eq!(p.streams[1].clock_rate, 90_000);
        assert!(p.streams[1].metadata.is_none());

        let mut session_id = None;
        super::parse_setup(
            response(include_bytes!("testdata/hikvision_setup.txt")),
            &mut session_id,
            &mut p.streams[0]
        ).unwrap();
        assert_eq!(session_id, Some("2115183928".to_owned()));
        assert_eq!(p.streams[0].ssrc, Some(0x63c096a4));
    }

    #[test]
    fn reolink() {
        let mut p = parse_describe(
            "rtsp://192.168.5.206:554/h264Preview_01_main",
            include_bytes!("testdata/reolink_describe.txt")).unwrap();
        assert_eq!(
            p.base_url.as_str(),
            "rtsp://192.168.5.206/h264Preview_01_main/");
        assert!(!p.accept_dynamic_rate);

        assert_eq!(p.streams.len(), 2);

        // H.264 video stream.
        assert_eq!(p.streams[0].control, "trackID=1");
        assert_eq!(p.streams[0].media, "video");
        assert_eq!(p.streams[0].encoding_name, "H264");
        assert_eq!(p.streams[0].rtp_payload_type, 96);
        assert_eq!(p.streams[0].clock_rate, 90_000);
        let metadata = p.streams[0].metadata.as_ref().unwrap();
        assert_eq!(metadata.rfc6381_codec(), "avc1.640033");
        assert_eq!(metadata.pixel_dimensions(), (2560, 1440));
        assert_eq!(metadata.pixel_aspect_ratio(), None);
        assert_eq!(metadata.frame_rate(), None);

        // audio stream
        assert_eq!(p.streams[1].control, "trackID=2");
        assert_eq!(p.streams[1].media, "audio");
        assert_eq!(p.streams[1].encoding_name, "MPEG4-GENERIC");
        assert_eq!(p.streams[1].rtp_payload_type, 97);
        assert_eq!(p.streams[1].clock_rate, 16_000);
        assert!(p.streams[1].metadata.is_none());

        let mut session_id = None;
        super::parse_setup(
            response(include_bytes!("testdata/reolink_setup.txt")),
            &mut session_id,
            &mut p.streams[0]
        ).unwrap();
        assert_eq!(session_id, Some("F8F8E425".to_owned()));
        assert_eq!(p.streams[0].ssrc, None);
    }
}