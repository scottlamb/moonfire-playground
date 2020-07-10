Experiments with NVR-based video analytics.

Currently the `foo` executable uses ffmpeg, TensorFlow Lite, and a [Coral USB
Accelerator](https://coral.ai/products/accelerator/) to perform object
detection on a single `.mp4`, producing a WebVTT metadata file that can be
viewed in-browser to show the annotations. See `viewer.html`.

This requires building the TensorFlow Lite C API.

## Installation

Some hints on installing:

https://coral.ai/docs/accelerator/get-started#on-linux
https://github.com/bazelbuild/bazelisk

```shell
sudo apt-get install libswscale-dev

echo "deb https://packages.cloud.google.com/apt coral-edgetpu-stable main" | \
sudo tee /etc/apt/sources.list.d/coral-edgetpu.list

curl https://packages.cloud.google.com/apt/doc/apt-key.gpg | \
sudo apt-key add

sudo apt-get update

sudo apt-get install libedgetpu1-std libedgetpu-dev

# Build the TensorFlow Lite C API at the required commit to match the
# installed Edge TPU library.
# https://github.com/google-coral/edgetpu/issues/44#issuecomment-589170013
# https://github.com/google-coral/edgetpu/blob/master/WORKSPACE#L5
cd ~/git
git clone https://github.com/tensorflow/tensorflow
cd tensorflow
git checkout d855adfc5a0195788bf5f92c3c7352e638aa1109
./configure
bazel build -c opt //tensorflow/lite/c:tensorflowlite_c
sudo install -m 755 bazel-bin/tensorflow/lite/c/libtensorflowlite_c.so /usr/local/lib
sudo ldconfig

cd ~/git/moonfire-playground/nvr-analytics
cargo build --release
```

To run the backfill command, create a dummy database:

```
sqlite3 mydb < src/schema.sql
```

Create a suitable Moonfire NVR cookie via:

```
sudo -u moonfire-nvr nvr login --permissions='view_video: true' $USER
```

and pass it (`s=` and onward) to backfill's `cookie` argument.

```
RUST_BACKTRACE=1 target/release/backfill --db=./mydb --cookie=s=... --nvr=http://localhost:8080
```

Currently expects a Moonfire NVR from the `new-schema` branch (not `master`).

## Future Work

I'd like this to be a processor that connects to Moonfire NVR, subscribes to
new video segments and annotates them, as well as backfilling old segments.
There's no Moonfire NVR schema, API, or UI for object detection yet.

It currently does H.264 decoding and scaling/colorspace transformation in
software then TensorFlow analysis on the USB accelerator, all in serial.

The H.264 decoding (and possibly scaling) could happen in hardware for a
significant speed-up. On an Intel machine where the current user is in the
`video` group, compare:

```
time ffmpeg -i input.mp4 -f null -
time ffmpeg -hwaccel vaapi -hwaccel_device /dev/dri/renderD128 -i input.mp4 -f null -
```

Similar speed-up should be possible on a Raspberry Pi although the stock ffmpeg
doesn't include acceleration support.

H.264 decoding and TensorFlow analysis could happen in separate threads to
speed up decoding a single video and/or multiple videos could be decoded in
separate threads to speed up bulk operations.

This uses the pretrained MobileNet SSD v2 (COCO) model from
https://coral.ai/models/. Quality could probably be improved significantly by
using transfer learning to train on outdoor surveillance images in a variety
of weather conditions, day vs night mode, etc.
