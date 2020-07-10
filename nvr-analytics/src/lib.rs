use std::str::FromStr;

pub static MODEL: &'static [u8] = include_bytes!("model.tflite");

pub static LABELS: [Option<&'static str>; 90] = [
    Some("person"),
    Some("bicycle"),
    Some("car"),
    Some("motorcycle"),
    Some("airplane"),
    Some("bus"),
    Some("train"),
    Some("truck"),
    Some("boat"),
    Some("traffic light"),
    Some("fire hydrant"),
    None,
    Some("stop sign"),
    Some("parking meter"),
    Some("bench"),
    Some("bird"),
    Some("cat"),
    Some("dog"),
    Some("horse"),
    Some("sheep"),
    Some("cow"),
    Some("elephant"),
    Some("bear"),
    Some("zebra"),
    Some("giraffe"),
    None,
    Some("backpack"),
    Some("umbrella"),
    None,
    None,
    Some("handbag"),
    Some("tie"),
    Some("suitcase"),
    Some("frisbee"),
    Some("skis"),
    Some("snowboard"),
    Some("sports ball"),
    Some("kite"),
    Some("baseball bat"),
    Some("baseball glove"),
    Some("skateboard"),
    Some("surfboard"),
    Some("tennis racket"),
    Some("bottle"),
    None,
    Some("wine glass"),
    Some("cup"),
    Some("fork"),
    Some("knife"),
    Some("spoon"),
    Some("bowl"),
    Some("banana"),
    Some("apple"),
    Some("sandwich"),
    Some("orange"),
    Some("broccoli"),
    Some("carrot"),
    Some("hot dog"),
    Some("pizza"),
    Some("donut"),
    Some("cake"),
    Some("chair"),
    Some("couch"),
    Some("potted plant"),
    Some("bed"),
    None,
    Some("dining table"),
    None,
    None,
    Some("toilet"),
    None,
    Some("tv"),
    Some("laptop"),
    Some("mouse"),
    Some("remote"),
    Some("keyboard"),
    Some("cell phone"),
    Some("microwave"),
    Some("oven"),
    Some("toaster"),
    Some("sink"),
    Some("refrigerator"),
    None,
    Some("book"),
    Some("clock"),
    Some("vase"),
    Some("scissors"),
    Some("teddy bear"),
    Some("hair drier"),
    Some("toothbrush"),
];

pub fn init_logging() -> mylog::Handle {
    let h = mylog::Builder::new()
        .set_format(::std::env::var("MOONFIRE_FORMAT")
                    .map_err(|_| ())
                    .and_then(|s| mylog::Format::from_str(&s))
                    .unwrap_or(mylog::Format::Google))
        .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
        .build();
    h.clone().install().unwrap();
    h
}

/// Copies from a RGB24 VideoFrame to a 1xHxWx3 Tensor.
pub fn copy(from: &moonfire_ffmpeg::avutil::VideoFrame, to: &mut moonfire_tflite::Tensor) {
    let from = from.plane(0);
    let to = to.bytes_mut();
    let (w, h) = (from.width, from.height);
    let mut from_i = 0;
    let mut to_i = 0;
    for _y in 0..h {
        to[to_i..to_i+3*w].copy_from_slice(&from.data[from_i..from_i+3*w]);
        from_i += from.linesize;
        to_i += 3*w;
    }
}

pub fn label(class: f32) -> Option<&'static str> {
    let class = class as usize;  // TODO: better way to do this?
    if class < LABELS.len() {
        LABELS[class]
    } else {
        None
    }
}
