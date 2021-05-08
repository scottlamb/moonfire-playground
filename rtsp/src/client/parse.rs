use bytes::{Buf, Bytes};
use failure::{Error, ResultExt, bail, format_err};
use log::debug;
use sdp::media_description::MediaDescription;
use url::Url;
use std::convert::TryFrom;

use super::{Presentation, Stream};

pub(crate) fn join_control(base_url: &Url, control: &str) -> Result<Url, Error> {
    //let control_value = control_value.ok_or_else(|| format_err!("control attribute has no value"))?;
    if control == "*" {
        return Ok(base_url.clone());
    }
    Ok(base_url.join(control).with_context(|_| {
        format_err!("unable to join base url {} with control url {:?}", base_url, control)
    })?)
}

/// Returns the `CSeq` from an RTSP response as a `u32`, or `None` if missing/unparseable.
pub(crate) fn get_cseq(response: &rtsp_types::Response<Bytes>) -> Option<u32> {
    response.header(&rtsp_types::headers::CSEQ)
            .and_then(|cseq| u32::from_str_radix(cseq.as_str(), 10).ok())
}

/// Splits the string on the first occurrence of the specified delimiter and
/// returns prefix before delimiter and suffix after delimiter.
///
/// This matches [str::split_once](https://doc.rust-lang.org/std/primitive.str.html#method.split_once)
/// but doesn't require nightly.
pub(crate) fn split_once(str: &str, delimiter: char) -> Option<(&str, &str)> {
    str.find(delimiter).map(|p| (&str[0..p], &str[p+1..]))
}

/// Parses a [MediaDescription] to a [Stream].
/// On failure, returns an error which is expected to be supplemented with
/// the [MediaDescription] debug string.
fn parse_media(base_url: &Url, media_description: &MediaDescription) -> Result<Stream, Error> {
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
            control = a.value.as_deref().map(|c| join_control(base_url, c)).transpose()?;
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
        state: super::StreamState::Uninit,
    })
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

    // https://tools.ietf.org/html/rfc2326#appendix-C.1.1
    let base_url = response.header(&rtsp_types::headers::CONTENT_BASE)
        .or_else(|| response.header(&rtsp_types::headers::CONTENT_LOCATION))
        .map(|v| Url::parse(v.as_str()))
        .unwrap_or(Ok(request_url))?;

    let mut control = None;
    for a in &sdp.attributes {
        if a.key == "control" {
            control = a.value.as_deref().map(|c| join_control(&base_url, c)).transpose()?;
            break;
        }
    }
    let control = control.ok_or_else(|| format_err!("no control url"))?;

    let streams = sdp.media_descriptions
        .iter()
        .enumerate()
        .map(|(i, m)| parse_media(&base_url, &m)
            .with_context(|_| format!("Unable to parse stream {}: {:#?}", i, &m))
            .map_err(Error::from))
        .collect::<Result<Vec<Stream>, Error>>()?;

    let accept_dynamic_rate = matches!(response.header(&crate::X_ACCEPT_DYNAMIC_RATE), Some(h) if h.as_str() == "1");
    
    Ok(Presentation {
        streams,
        accept_dynamic_rate,
        base_url,
        control,
        sdp,
    })
}

pub(crate) struct SetupResponse<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) ssrc: Option<u32>,
    pub(crate) channel_id: u8,
}

/// Parses a `SETUP` response.
/// `session_id` is checked for assignment or reassignment.
/// Returns an assigned interleaved channel id (implying the next channel id
/// is also assigned) or errors.
pub(crate) fn parse_setup(response: &rtsp_types::Response<Bytes>) -> Result<SetupResponse, Error> {
    let session = response.header(&rtsp_types::headers::SESSION)
        .ok_or_else(|| format_err!("SETUP response has no Session header"))?;
    let session_id = match session.as_str().find(';') {
        None => session.as_str(),
        Some(i) => &session.as_str()[..i],
    };
    let transport = response.header(&rtsp_types::headers::TRANSPORT)
        .ok_or_else(|| format_err!("SETUP response has no Transport header"))?;
    let mut channel_id = None;
    let mut ssrc = None;
    for part in transport.as_str().split(';') {
        if let Some(v) = part.strip_prefix("ssrc=") {
            let v = u32::from_str_radix(v, 16).map_err(|_| format_err!("Unparseable ssrc {}", v))?;
            ssrc = Some(v);
            break;
        } else if let Some(interleaved) = part.strip_prefix("interleaved=") {
            let mut channels = interleaved.splitn(2, '-');
            let n = channels.next().expect("splitn returns at least one part");
            let n = u8::from_str_radix(n, 10).map_err(|_| format_err!("bad channel number {}", n))?;
            if let Some(m) = channels.next() {
                let m = u8::from_str_radix(m, 10)
                    .map_err(|_| format_err!("bad second channel number {}", m))?;
                if n.checked_add(1) != Some(m) {
                    bail!("Expected adjacent channels; got {}-{}", n, m);
                }
            }
            channel_id = Some(n);
        }
    }
    let channel_id = channel_id
        .ok_or_else(|| format_err!("SETUP response Transport header has no interleaved parameter"))?;
    Ok(SetupResponse {
        session_id,
        channel_id,
        ssrc,
    })
}

pub(crate) fn parse_play(
    response: rtsp_types::Response<Bytes>,
    presentation: &mut Presentation,
) -> Result<(), Error> {
    // https://tools.ietf.org/html/rfc2326#section-12.33
    let rtp_info = response.header(&rtsp_types::headers::RTP_INFO)
        .ok_or_else(|| format_err!("PLAY response has no RTP-Info header"))?;
    for s in rtp_info.as_str().split(',') {
        let s = s.trim();
        let mut parts = s.split(';');
        let url = parts
            .next()
            .expect("split always returns at least one part")
            .strip_prefix("url=")
            .ok_or_else(|| format_err!("RTP-Info missing stream URL"))?;
        let url = join_control(&presentation.base_url, url)?;
        let stream = presentation.streams
            .iter_mut()
            .find(|s| s.control == url)
            .ok_or_else(|| format_err!("can't find RTP-Info stream {}", url))?;
        let state = match &mut stream.state {
            super::StreamState::Uninit => {
                // This appears to happen for Reolink devices. It also happens in some of other the
                // tests here simply because I didn't include all the SETUP steps.
                debug!("PLAY response described stream {} in Uninit state", &stream.control);
                continue;
            },
            super::StreamState::Init(init) => init,
            super::StreamState::Playing { .. } => unreachable!(),
        };
        for part in parts {
            let (key, value) = split_once(part, '=')
                .ok_or_else(|| format_err!("RTP-Info param has no ="))?;
            match key {
                "seq" => {
                    let seq = u16::from_str_radix(value, 10)
                        .map_err(|_| format_err!("bad seq {:?}", value))?;
                    state.initial_seq = Some(seq);
                },
                "rtptime" => {
                    let rtptime = u32::from_str_radix(value, 10)
                        .map_err(|_| format_err!("bad rtptime {:?}", value))?;
                    state.initial_rtptime = Some(rtptime);
                },
                "ssrc" => {
                    let ssrc = u32::from_str_radix(value, 16)
                        .map_err(|_| format_err!("Unparseable ssrc {}", value))?;
                    state.ssrc = Some(ssrc);
                },
                _ => {},
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use failure::Error;
    use url::Url;

    use crate::client::{StreamStateInit, video::Metadata};

    use super::super::StreamState;

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
        // DESCRIBE.
        let mut p = parse_describe(
            "rtsp://192.168.5.111:554/cam/realmonitor?channel=1&subtype=1&unicast=true&proto=Onvif",
            include_bytes!("testdata/dahua_describe_h264_aac_onvif.txt")).unwrap();
        assert_eq!(
            p.base_url.as_str(),
            "rtsp://192.168.5.111:554/cam/realmonitor?channel=1&subtype=1&unicast=true&proto=Onvif/");
        assert!(p.accept_dynamic_rate);

        assert_eq!(p.streams.len(), 3);

        // H.264 video stream.
        //assert_eq!(p.streams[0].control, "trackID=0");
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
        //assert_eq!(p.streams[1].control, "trackID=1");
        assert_eq!(p.streams[1].media, "audio");
        assert_eq!(p.streams[1].encoding_name, "MPEG4-GENERIC");
        assert_eq!(p.streams[1].rtp_payload_type, 97);
        assert_eq!(p.streams[1].clock_rate, 48_000);
        assert!(p.streams[1].metadata.is_none());

        // ONVIF metadata stream.
        //assert_eq!(p.streams[2].control, "trackID=4");
        assert_eq!(p.streams[2].media, "application");
        assert_eq!(p.streams[2].encoding_name, "vnd.onvif.metadata");
        assert_eq!(p.streams[2].rtp_payload_type, 107);
        assert_eq!(p.streams[2].clock_rate, 90_000);
        assert!(p.streams[2].metadata.is_none());

        // SETUP.
        let setup_response = response(include_bytes!("testdata/dahua_setup.txt"));
        let setup_response = super::parse_setup(&setup_response).unwrap();
        assert_eq!(setup_response.session_id, "634214675641");
        assert_eq!(setup_response.channel_id, 0);
        assert_eq!(setup_response.ssrc, Some(0x30a98ee7));
        p.streams[0].state = StreamState::Init(StreamStateInit {
            ssrc: setup_response.ssrc,
            initial_seq: None,
            initial_rtptime: None,
        });

        // PLAY.
        super::parse_play(
            response(include_bytes!("testdata/dahua_play.txt")),
            &mut p
        ).unwrap();
        match &p.streams[0].state {
            StreamState::Init(s) => {
                assert_eq!(s.initial_seq, Some(47121));
                assert_eq!(s.initial_rtptime, Some(3475222385));
            },
            _ => panic!(),
        };
        // The other streams don't get filled in because they're in state Uninit.
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
        // DESCRIBE.
        let mut p = parse_describe(
            "rtsp://192.168.5.106:554/Streaming/Channels/101?transportmode=unicast&Profile=Profile_1",
            include_bytes!("testdata/hikvision_describe.txt")).unwrap();
        assert_eq!(
            p.base_url.as_str(),
            "rtsp://192.168.5.106:554/Streaming/Channels/101/");
        assert!(!p.accept_dynamic_rate);

        assert_eq!(p.streams.len(), 2);

        // H.264 video stream.
        //assert_eq!(p.streams[0].control, "rtsp://192.168.5.106:554/Streaming/Channels/101/trackID=1?transportmode=unicast&profile=Profile_1");
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
        //assert_eq!(p.streams[1].control, "rtsp://192.168.5.106:554/Streaming/Channels/101/trackID=3?transportmode=unicast&profile=Profile_1");
        assert_eq!(p.streams[1].media, "application");
        assert_eq!(p.streams[1].encoding_name, "vnd.onvif.metadata");
        assert_eq!(p.streams[1].rtp_payload_type, 107);
        assert_eq!(p.streams[1].clock_rate, 90_000);
        assert!(p.streams[1].metadata.is_none());

        // SETUP.
        let setup_response = response(include_bytes!("testdata/hikvision_setup.txt"));
        let setup_response = super::parse_setup(&setup_response).unwrap();
        assert_eq!(setup_response.session_id, "708345999");
        assert_eq!(setup_response.channel_id, 0);
        assert_eq!(setup_response.ssrc, Some(0x4cacc3d1));
        p.streams[0].state = StreamState::Init(StreamStateInit {
            ssrc: setup_response.ssrc,
            initial_seq: None,
            initial_rtptime: None,
        });

        // PLAY.
        super::parse_play(
            response(include_bytes!("testdata/hikvision_play.txt")),
            &mut p
        ).unwrap();
        match p.streams[0].state {
            StreamState::Init(state) => {
                assert_eq!(state.initial_seq, Some(24104));
                assert_eq!(state.initial_rtptime, Some(1270711678));
            },
            _ => panic!(),
        }
        // The other stream isn't filled in because it's in state Uninit.
    }

    #[test]
    fn reolink() {
        // DESCRIBE.
        let mut p = parse_describe(
            "rtsp://192.168.5.206:554/h264Preview_01_main",
            include_bytes!("testdata/reolink_describe.txt")).unwrap();
        assert_eq!(
            p.base_url.as_str(),
            "rtsp://192.168.5.206/h264Preview_01_main/");
        assert!(!p.accept_dynamic_rate);

        assert_eq!(p.streams.len(), 2);

        // H.264 video stream.
        //assert_eq!(p.streams[0].control, "trackID=1");
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
        //assert_eq!(p.streams[1].control, "trackID=2");
        assert_eq!(p.streams[1].media, "audio");
        assert_eq!(p.streams[1].encoding_name, "MPEG4-GENERIC");
        assert_eq!(p.streams[1].rtp_payload_type, 97);
        assert_eq!(p.streams[1].clock_rate, 16_000);
        assert!(p.streams[1].metadata.is_none());

        // SETUP.
        let setup_response = response(include_bytes!("testdata/reolink_setup.txt"));
        let setup_response = super::parse_setup(&setup_response).unwrap();
        assert_eq!(setup_response.session_id, "F8F8E425");
        assert_eq!(setup_response.channel_id, 0);
        assert_eq!(setup_response.ssrc, None);
        p.streams[0].state = StreamState::Init(StreamStateInit::default());
        p.streams[1].state = StreamState::Init(StreamStateInit::default());

        // PLAY.
        super::parse_play(
            response(include_bytes!("testdata/reolink_play.txt")),
            &mut p
        ).unwrap();
        match p.streams[0].state {
            StreamState::Init(state) => {
                assert_eq!(state.initial_seq, Some(16852));
                assert_eq!(state.initial_rtptime, Some(1070938629));
            },
            _ => panic!(),
        };
        match p.streams[1].state {
            StreamState::Init(state) => {
                assert_eq!(state.initial_rtptime, Some(3075976528));
                assert_eq!(state.ssrc, Some(0x9fc9fff8));
            },
            _ => panic!(),
        };
    }
}
