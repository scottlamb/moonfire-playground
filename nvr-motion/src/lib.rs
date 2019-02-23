extern crate moonfire_ffmpeg;

const Y_NOISE_LEVEL: i16 = 32;

pub trait Processor {
    fn process(&mut self, frame: &moonfire_ffmpeg::Frame) -> Result<(), &str>;
}

/// Processes using an algorithm similar to the motion project.
pub struct MotionProcessor {
    // just the y plane.
    ref_img: Vec<u8>,

    /// TODO: grayscale sensitivity mask; white means full, black means none.
    // mask: moonfire_ffmpeg::Image,

    // TODO: describe layout. better to use an Image?
    // one bit per pixel; 1 means changed. (aka PIX_FMT_MONOBLACK)
    diffs: Vec<u8>,
    //diffs: moonfire_ffmpeg::Frame,

    /// number of pixels set in `diffs`.
    num_diffs: usize,
}

impl MotionProcessor {
    pub fn new(initial_frame: &moonfire_ffmpeg::Frame) -> Self {
        let (w, h) = (initial_frame.width(), initial_frame.height());
        let pix_fmt = initial_frame.pix_fmt();
        // TODO: ensure yuv pixel format.
        let mut diffs = Vec::new();
        diffs.resize((w as usize * h as usize + 7) / 8, 0);
        MotionProcessor {
            ref_img: initial_frame.plane(0).data.to_owned(),
            diffs,
            num_diffs: 0,
        }
    }

    /// Diffs the given frame with the reference frame, updating the mask and 
    fn diff(&mut self, frame: &moonfire_ffmpeg::Frame) {
        let y = frame.plane(0);
        //println!("y.linesize={} y.data.len={} y.width={} y.height={}",
        //         y.linesize, y.data.len(), y.width, y.height);

        let mut i = 0;
        let mut n = 0;
        while i < y.data.len() - 7 {
            let mut d = 0u8;
            let d0 = (y.data[i+0] as i16 - self.ref_img[i+0] as i16).abs() > Y_NOISE_LEVEL;
            let d1 = (y.data[i+1] as i16 - self.ref_img[i+1] as i16).abs() > Y_NOISE_LEVEL;
            let d2 = (y.data[i+2] as i16 - self.ref_img[i+2] as i16).abs() > Y_NOISE_LEVEL;
            let d3 = (y.data[i+3] as i16 - self.ref_img[i+3] as i16).abs() > Y_NOISE_LEVEL;
            let d4 = (y.data[i+4] as i16 - self.ref_img[i+4] as i16).abs() > Y_NOISE_LEVEL;
            let d5 = (y.data[i+5] as i16 - self.ref_img[i+5] as i16).abs() > Y_NOISE_LEVEL;
            let d6 = (y.data[i+6] as i16 - self.ref_img[i+6] as i16).abs() > Y_NOISE_LEVEL;
            let d7 = (y.data[i+7] as i16 - self.ref_img[i+7] as i16).abs() > Y_NOISE_LEVEL;
            d =  (d0 as u8) |
                ((d1 as u8) << 1) |
                ((d2 as u8) << 2) |
                ((d3 as u8) << 3) |
                ((d4 as u8) << 4) |
                ((d5 as u8) << 5) |
                ((d6 as u8) << 6) |
                ((d7 as u8) << 7);
            self.diffs[i >> 3] = d;
            n += (d0 as usize) + (d1 as usize) + (d2 as usize) + (d3 as usize) +
                 (d4 as usize) + (d5 as usize) + (d6 as usize) + (d7 as usize);
            i += 8;
        }
        assert!(i == y.data.len());  // TODO: process leftovers.
        self.num_diffs = n;
        println!("diffs: {}", n);
        self.ref_img.copy_from_slice(&y.data);
    }
}

impl Processor for MotionProcessor {
    fn process(&mut self, frame: &moonfire_ffmpeg::Frame) -> Result<(), &str> {
        //if frame.width() != self.ref_img.w || frame.height() != self.ref_img.h ||
        //   frame.pix_fmt() != self.ref_img.pix_fmt {
        //    return Err("changed format");
        //}

        self.diff(frame);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
