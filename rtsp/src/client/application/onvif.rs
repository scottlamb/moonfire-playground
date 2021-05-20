//! ONVIF metadata streams.
//! See the
//! [ONVIF Streaming Specification](https://www.onvif.org/specs/stream/ONVIF-Streaming-Spec.pdf)
//! version 19.12 section 5.2.1.1. The RTP layer muxing is simple: RTP packets with the MARK
//! bit set end messages.

use async_trait::async_trait;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use failure::{Error, bail};

#[async_trait]
pub trait MessageHandler {
    async fn message(&mut self, timestamp: crate::Timestamp, msg: Bytes) -> Result<(), Error>;
}

pub struct Handler<M: MessageHandler + Send> {
    in_progress: Option<(crate::Timestamp, BytesMut)>,
    high_water_size: usize,
    inner: M,
}

impl<M: MessageHandler + Send> Handler<M> {
    pub fn new(inner: M) -> Self {
        Handler {
            in_progress: None,
            high_water_size: 0,
            inner,
        }
    }

    pub async fn pkt(&mut self, pkt: crate::client::rtp::Packet) -> Result<(), failure::Error> {
        if let Some((timestamp, mut buf)) = self.in_progress.take() {
            if timestamp.timestamp != pkt.timestamp.timestamp {
                bail!("Timestamp changed from {} to {} (seq {:04x} with message in progress",
                      &timestamp, &pkt.timestamp, pkt.sequence_number);
            }
            buf.put(pkt.payload);
            if pkt.mark {
                self.high_water_size = std::cmp::max(self.high_water_size, buf.remaining());
                return self.inner.message(timestamp, buf.freeze()).await;
            }
            self.in_progress = Some((timestamp, buf));
        } else {
            if pkt.mark {
                return self.inner.message(pkt.timestamp, pkt.payload).await;
            }
            let mut buf = BytesMut::with_capacity(std::cmp::max(self.high_water_size, 2 * pkt.payload.remaining()));
            buf.put(pkt.payload);
            self.in_progress = Some((pkt.timestamp, buf));
        }
        Ok(())
    }

    pub async fn end(&mut self) -> Result<(), failure::Error> {
        todo!()
    }
}
