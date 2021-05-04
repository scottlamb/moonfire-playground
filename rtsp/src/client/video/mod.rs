use failure::Error;

pub mod h264;

pub trait VideoHandler {
    type Metadata : Metadata;
    fn metadata_change(&self, metadata: &Self::Metadata) -> Result<(), Error>;
    fn picture(&self, picture: Picture) -> Result<(), Error>;
}

pub trait Metadata : Clone + std::fmt::Debug {
    /// Returns a codec description in
    /// [RFC-6381](https://tools.ietf.org/html/rfc6381) form, eg `avc1.4D401E`.
    // TODO: use https://github.com/dholroyd/rfc6381-codec crate once published?
    fn rfc6381_codec(&self) -> &str;

    /// Returns the overall dimensions of the video frame in pixels, as `(width, height)`.
    fn pixel_dimensions(&self) -> (u32, u32);

    /// Returns the displayed size of a pixel, if known, as a dimensionless ratio `(h_spacing, v_spacing)`.
    /// This is as specified in [ISO/IEC 14496-12:2015](https://standards.iso.org/ittf/PubliclyAvailableStandards/c068960_ISO_IEC_14496-12_2015.zip])
    /// section 12.1.4.
    ///
    /// It's common for IP cameras to use [anamorphic](https://en.wikipedia.org/wiki/Anamorphic_format) sub streams.
    /// Eg a 16x9 camera may export the same video source as a 1920x1080 "main"
    /// stream and a 704x480 "sub" stream, without cropping. The former has a
    /// pixel aspect ratio of `(1, 1)` while the latter has a pixel aspect ratio
    /// of `(40, 33)`.
    fn pixel_aspect_ratio(&self) -> Option<(u32, u32)>;

    /// Returns the maximum frame rate in seconds as `(numerator, denominator)`,
    /// if known. Eg 15 frames per second is returned as `(1, 15)`, and the
    /// standard NTSC framerate (roughly 29.97 fps) is returned as
    /// `(30000, 1001)`.
    fn frame_rate(&self) -> Option<(u32, u32)>;
}

/// A single encoded picture (aka video frame or sample).
/// Use the [bytes::Buf] implementation to retrieve data. Durations aren't
/// specified here; they can be calculated from the timestamp of a following
/// picture, or approximated via the frame rate.
pub struct Picture {
    /// This picture's timestamp in the time base associated with the stream.
    pub rtp_timestamp: crate::Timestamp,

    /// If this is a "random access point (RAP)" aka "instantaneous decoding refresh (IDR)" picture.
    /// The former is defined in ISO/IEC 14496-12; the latter in H.264. Both mean that this picture
    /// can be decoded without any other AND no pictures following this one depend on any pictures
    /// before this one.
    pub is_random_access_point: bool,

    /// If no other pictures require this one to be decoded correctly.
    /// In H.264 terms, this is a frame with `nal_ref_idc == 0`.
    pub is_disposable: bool,

    /// Position within `concat(data_prefix, data)`.
    pos: u32,

    data_prefix: [u8; 4],

    /// Frame content in the requested format. Currently in a single [bytes::Bytes]
    /// allocation, but this may change when supporting H.264 partitioned slices
    /// or if we revise the fragmentation implementation.
    data: bytes::Bytes,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        //use pretty_hex::PrettyHex;
        f.debug_struct("Frame")
         .field("rtp_timestamp", &self.rtp_timestamp)
         .field("is_random_access_point", &self.is_random_access_point)
         .field("is_disposable", &self.is_disposable)
         .field("pos", &self.pos)
         .field("data_len", &(self.data.len() + 4))
         //.field("data", &self.data.hex_dump()) 
         .finish()
    }
}

// FIXME: this should have the prefix for H.264 NALs. I don't want to add it to the Bytes,
// and that is codec-specific, so maybe Picture should be a trait object also?
impl bytes::Buf for Picture {
    fn remaining(&self) -> usize {
        self.data.len() + 4 - (self.pos as usize)
    }

    fn chunk(&self) -> &[u8] {
        let pos = self.pos as usize;
        if let Some(pos_within_data) = pos.checked_sub(4) {
            &self.data[pos_within_data..]
        } else {
            &self.data_prefix[pos..]
        }
    }

    fn advance(&mut self, cnt: usize) {
        assert!((self.pos as usize) + cnt <= 4 + self.data.len());
        self.pos += cnt as u32;
    }

    fn chunks_vectored<'a>(&'a self, dst: &mut [std::io::IoSlice<'a>]) -> usize {
        match dst.len() {
            0 => 0,
            1 => {
                dst[0] = std::io::IoSlice::new(self.chunk());
                1
            },
            _ if self.pos < 4 => {
                dst[0] = std::io::IoSlice::new(&self.data_prefix[self.pos as usize..]);
                dst[1] = std::io::IoSlice::new(&self.data);
                2
            },
            _ => {
                dst[0] = std::io::IoSlice::new(&self.data[(self.pos - 4) as usize..]);
                1
            }
        }
    }
}
