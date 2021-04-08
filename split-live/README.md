Debugging tool to split parts out of a live stream into the local filesystem. Run like so:

```
$ RUST_BACKTRACE=1 cargo run -- \
      --cookie=s=... \
      --url=wss://.../api/cameras/.../sub/live.m4s
```

for each part, it will create `msgN.headers` and `msgN.m4s`.

See [API docs for `GET /api/cameras/<uuid>/<stream>/live.m4s`](https://github.com/scottlamb/moonfire-nvr/blob/master/design/api.md#get-apicamerasuuidstreamlivem4s).

To remove parts afterward:

```
$ rm -f msg*.{m4s,headers}
```
