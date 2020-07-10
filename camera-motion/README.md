A crude attempt at on-camera motion detection. Needs a Moonfire NVR
installation on the `new-schema` branch. The database must be populated with
signals of the following types:

| watcher type          | uuid                                   |
| --------------------- | -------------------------------------- |
| rtsp metadata stream  | `5684523f-f29d-42e9-b6af-1e123f2b76fb` |
| hikvision proprietary | `18bf0756-2120-4fbc-99d1-a367b10ef297` |
| dahua proprietary     | `ee66270f-d9c6-4819-8b33-9720d4cbca6b` |

as by running `moonfire-nvr sql` with the following SQL that assumes the camera
called `driveway` is a Dahua and the others are Hikvisions:

```sql
delete from signal_change;
delete from signal_type_enum;
delete from signal_camera;
delete from signal;

insert into signal (id, source_uuid, type_uuid, short_name)
select
  id,
  uuid,
  case short_name when 'driveway' then x'EE66270FD9C648198B339720D4CBCA6B'
                                  else x'18BF075621204FBC99D1A367B10EF297' end,
  short_name
from
  camera;

insert into signal_type_enum (type_uuid, value, name, motion, color)
   values (x'EE66270FD9C648198B339720D4CBCA6B', 1, 'off', 0, 'black'),
          (x'EE66270FD9C648198B339720D4CBCA6B', 2, 'on', 1, 'red'),
          (x'18BF075621204FBC99D1A367B10EF297', 1, 'off', 0, 'black'),
          (x'18BF075621204FBC99D1A367B10EF297', 2, 'on', 1, 'red');

insert into signal_camera (signal_id, camera_id, type)
select id, id, 0 from camera;
```

Also needs a cookie as generated via the following command:

```
$ moonfire-nvr login --permissions='read_camera_configs: true
                                    update_signals: true' $USER
```

Save the output line that starts with `s=`; pass it via the `--cookie`
commandlne argument.

Example `/etc/systemd/system/camera-motion.service` file:

```
Unit]
Description=Moonfire NVR on-camera motion detection
After=moonfire-nvr.target

[Service]
ExecStart=/usr/local/bin/camera-motion \
    --cookie=s=... \
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
$ sudo systemctl start camera-motion
$ sudo systemctl enable camera-motion
```
