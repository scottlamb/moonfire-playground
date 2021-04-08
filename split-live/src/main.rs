use structopt::StructOpt;

#[derive(StructOpt)]
struct Opt {
    #[structopt(short, long, parse(try_from_str))]
    cookie: Option<http::header::HeaderValue>,

    #[structopt(short, long, parse(try_from_str))]
    url: http::Uri,
}

fn main() {
    let opt = Opt::from_args();
    let mut builder = tungstenite::handshake::client::Request::builder().uri(opt.url);
    if let Some(c) = opt.cookie {
        builder = builder.header(http::header::COOKIE, c);
    }
    let (mut ws, _) = tungstenite::client::connect(builder.body(()).unwrap()).unwrap();
    for i in 0.. {
        let msg = ws.read_message().unwrap();
        let data = match &msg {
            tungstenite::Message::Binary(ref d) => d,
            tungstenite::Message::Ping(_) => continue,
            o @ _ => panic!("other data: {:?}", o),
        };
        let mut hdrs = [httparse::EMPTY_HEADER; 16];
        let (header_size, _) = httparse::parse_headers(data, &mut hdrs).unwrap().unwrap();
        println!("writing msg {}", i);
        std::fs::write(format!("msg{}.headers", i), &data[0..header_size]).unwrap();
        std::fs::write(format!("msg{}.m4s", i), &data[header_size..]).unwrap();
    }
}
