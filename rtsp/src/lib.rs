use bytes::{Buf, BufMut, Bytes, BytesMut};
use failure::bail;
use once_cell::sync::Lazy;
use rtsp_types::Message;

pub mod client;

pub static X_ACCEPT_DYNAMIC_RATE: Lazy<rtsp_types::HeaderName> = Lazy::new(
    || rtsp_types::HeaderName::from_static_str("x-Accept-Dynamic-Rate").expect("is ascii")
);
pub static X_DYNAMIC_RATE: Lazy<rtsp_types::HeaderName> = Lazy::new(
    || rtsp_types::HeaderName::from_static_str("x-Dynamic-Rate").expect("is ascii")
);

struct Codec {}

fn map_body<Body, NewBody: AsRef<[u8]>, F: FnOnce(Body) -> NewBody>(m: Message<Body>, f: F) -> Message<NewBody> {
    match m {
        Message::Request(r) => Message::Request(r.map_body(f)),
        Message::Response(r) => Message::Response(r.map_body(f)),
        Message::Data(d) => Message::Data(d.map_body(f)),
    }
}

impl tokio_util::codec::Decoder for Codec {
    type Item = rtsp_types::Message<bytes::Bytes>;
    type Error = failure::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // TODO: zero-copy.
        let (msg, len): (Message<&[u8]>, _) = match Message::parse(src) {
            Ok((m, l)) => (m, l),
            Err(rtsp_types::ParseError::Error) => bail!("RTSP parse error"),
            Err(rtsp_types::ParseError::Incomplete) => return Ok(None),
        };
        let msg = map_body(msg, Bytes::copy_from_slice);
        src.advance(len);
        Ok(Some(msg))
    }
}

impl tokio_util::codec::Encoder<rtsp_types::Message<bytes::Bytes>> for Codec {
    type Error = failure::Error;

    fn encode(&mut self, item: rtsp_types::Message<bytes::Bytes>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let mut w = std::mem::replace(dst, BytesMut::new()).writer();
        item.write(&mut w).expect("bytes Writer is infallible");
        *dst = w.into_inner();
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
