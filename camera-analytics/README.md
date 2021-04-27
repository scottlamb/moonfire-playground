A crude attempt at on-camera analytics (such as motion detection).


The database must be populated with a motion signal type, as by running the
following through `moonfire-nvr sql`:

```sql
delete from signal_change;
delete from signal_type_enum;
delete from signal_camera;
delete from signal;

insert into signal_type_enum (type_uuid, value, name, motion, color)
   values (x'EE66270FD9C648198B339720D4CBCA6B', 1, 'off', 0, 'black'),
          (x'EE66270FD9C648198B339720D4CBCA6B', 2, 'on', 1, 'red');
```

and then each signal of interest must be added to the database. Eg, the signals
`foo`, `bar`, and `baz` might be added in the following way:

```
$ cat > print_insert.py <<'EOF'
#!/usr/bin/python3

import sys
import uuid

print('insert into signal (source_uuid, type_uuid, short_name) values')
insert_lines = [
    f"    (X'{uuid.uuid4().hex.upper()}', X'EE66270FD9C648198B339720D4CBCA6B', '{short_name}')"
    for short_name in sys.argv[1:]]
print(',\n'.join(insert_lines) + ';')
EOF
$ chmod a+rx print_insert.py
$ ./print_insert.py foo bar baz | moonfire-nvr sql
```

Also needs a cookie as generated via the following command:

```
$ moonfire-nvr login --permissions='read_camera_configs: true
                                    update_signals: true' $USER
```

Save the output line that starts with `s=`; pass it via the `--cookie`
commandline argument.

Add a configuration file similar to the following, pass its filename via
the `--config` commandline argument:

```json
[
    {
        "cameraName": "driveway",
        "type": "dahua",
        "signals": [
            {
                "signalName": "driveway",
                "type": "motion"
            },
        ]
    },
    {
        "cameraName": "west_side",
        "type": "hikvision",
        "signalName": "west_side"
    },
    {
        "cameraName": "back_west",
        "type": "rtsp"
    }
]
```

This creates a watcher of each supported type: `dahua`, `hikvision`, and `rtsp`. Camera
and signal names must match Moonfire NVR's configuration. ONVIF hostname, username/password,
and RTSP URLs are taken from Moonfire NVR's config rather than duplicated here.

`dahua` and `hikvision` watchers are only expected to work on cameras of the respective manufacturer.
`rtsp` supports any ONVIF-compliant camera which has been set up as described
below. You can configured both a camera-specific and `rtsp` watcher for the
same camera if desired.

Example `/etc/systemd/system/camera-analytics.service` file:

```
[Unit]
Description=Moonfire NVR on-camera analytics
After=moonfire-nvr.target

[Service]
ExecStart=/usr/local/bin/camera-analytics \
    --cookie=s=... \
    --cfg=... \
    --nvr=https:/...
Environment=MOONFIRE_FORMAT=google-systemd
Environment=MOONFIRE_LOG=info,camera_motion=debug
Environment=RUST_BACKTRACE=1
Type=simple
User=moonfire-nvr
Nice=-20
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Enable via

```
$ sudo systemctl daemon-reload
$ sudo systemctl start camera-analytics
$ sudo systemctl enable camera-analytics
```

# Dahua cameras

Dahua cameras support several signals per camera, including:

*   `"type": "motion"`: motion detection
    *    leave `motionType` unset for basic motion detection, or
         set it to `SmartMotionHuman` or `SmartMotionVehicle` to
         combine with object detection of humans or vehicles,
         respectively. Smart motion detection must be supported
         by the camera and enabled in the UI (see
         `Event > Smart Motion Detection`).
    *    leave `region` unset (to detect basic motion anywhere
         in the frame) or set it to match a region name configured
         in the UI (`Event > Video Detection > Motion Detection > Area`).
*   `"type": "ivs"`: Intelligent Video Detection. This must be
    the configured "Smart Plan" (in `Event > Smart Plan`) and a matching
    rule must be enabled (in `Event > IVS > Rule Config`).
    *    `ivsType` must be set to `CrossLineDetection` or `ParkingDetection`.
    *    `ruleName` should be set to the name of the rule.
    *    leave `objectType` unset or choose `Human` or `Vehicle` to limit
    *    to detection of that type of object.

# Hikvision cameras

Currently this only supports one signal per camera for basic motion detection.

TODO: support multiple regions.

# RTSP cameras

Currently this only logs stream events without actually setting any signals.

The ONVIF metadata stream must be enabled. Moonfire NVR doesn't (yet) have
a facility for doing so, but there are a couple tools you could try.

[python-onvif-zeep](https://github.com/FalkTannhaeuser/python-onvif-zeep):

```python
#!/usr/bin/env python3

from onvif import ONVIFCamera
import sys

ip = '192.168.5.1'
print('camera %s' % ip)
c = ONVIFCamera(ip, 80, 'admin', 'password')

c.create_devicemgmt_service()
c.create_media_service()

config = c.media.GetMetadataConfigurations()[0]
config.Analytics = True
req = c.media.create_type('SetMetadataConfiguration')
req.Configuration = config
req.ForcePersistence = True
c.media.SetMetadataConfiguration(req)

for profile in c.media.GetProfiles():
    print('profile %s' % profile.token)
    if not hasattr(profile, 'MetadataConfiguration'):
        c.media.AddMetadataConfiguration({
            'ProfileToken': profile.token,
            'ConfigurationToken': config.token,
        })
    resp = c.media.GetStreamUri({
        'StreamSetup': {'Stream': 'RTP-Unicast', 'Transport': {'Protocol': 'RTSP'}},
        'ProfileToken': profile.token,
    })
    print(resp.Uri)
```

[lumeohq/onvif-rs](https://github.com/lumeohq/onvif-rs):

```
$ cargo run --example camera --username admin --password asdf --uri http://192.168.5.1/ enable-analytics
```
