A couple utilities for debugging my RTSP library,
[retina](https://github.com/scottlamb/retina/).

Eg, the command below will create a SQLite3 database `timedump.db` with a bunch
of data about RTSP sessions: timestamps, packet loss, etc.

There are some queries in `timedump.sql`, you can export to CSV and post-process
with scripts, etc.

```
RUST_BACKTRACE=1 \
MOONFIRE_LOG=moonfire_rtsp=info,moonfire_rtsp::client::audio::aac=info,info \
cargo run --release -- \
    timedump \
    --username admin \
    --password redacted \
    --db timedump.db \
    'rtsp://192.168.5.101/Streaming/Channels/1?transportmode=unicast&profile=Profile_1' \
    'rtsp://192.168.5.101/Streaming/Channels/2?transportmode=unicast&profile=Profile_2' \
    'rtsp://192.168.5.102/Streaming/Channels/1?transportmode=unicast&profile=Profile_1' \
    'rtsp://192.168.5.102/Streaming/Channels/2?transportmode=unicast&profile=Profile_2' \
    'rtsp://192.168.5.104/Streaming/Channels/1?transportmode=unicast&profile=Profile_1' \
    'rtsp://192.168.5.104/Streaming/Channels/2?transportmode=unicast&profile=Profile_2' \
    'rtsp://192.168.5.106/Streaming/Channels/1?transportmode=unicast&profile=Profile_1' \
    'rtsp://192.168.5.106/Streaming/Channels/2?transportmode=unicast&profile=Profile_2' \
    'rtsp://192.168.5.107:88/videoMain' \
    'rtsp://192.168.5.107:88/videoSub' \
    'rtsp://cam-driveway/cam/realmonitor?channel=1&subtype=0&unicast=true&proto=Onvif' \
    'rtsp://cam-driveway/cam/realmonitor?channel=1&subtype=1&unicast=true&proto=Onvif' \
    'rtsp://cam-courtyard/cam/realmonitor?channel=1&subtype=0&unicast=true&proto=Onvif'
```