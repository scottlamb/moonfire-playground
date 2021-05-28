use failure::{Error, bail, format_err};
use std::{convert::TryFrom, num::NonZeroU32};

use crate::Timestamp;

const MAX_FORWARD_TIME_JUMP_SECS: u32 = 10;

/// Creates [Timestamp]s (which don't wrap and can be converted to NPT aka normal play time)
/// from 32-bit (wrapping) RTP timestamps.
#[derive(Debug)]
pub(super) struct Timeline {
    timestamp: i64,
    clock_rate: NonZeroU32,
    start: Option<u32>,
    max_forward_jump: i32,
}

impl Timeline {
    /// Creates a new timeline, erroring on crazy clock rates.
    pub(super) fn new(start: Option<u32>, clock_rate: u32) -> Result<Self, Error> {
        let clock_rate = NonZeroU32::new(clock_rate)
            .ok_or_else(|| format_err!("clock_rate=0 rejected to prevent division by zero"))?;
        let max_forward_jump = u64::from(MAX_FORWARD_TIME_JUMP_SECS) * u64::from(clock_rate.get());
        let max_forward_jump = i32::try_from(max_forward_jump)
            .map_err(|_| format_err!(
                "clock_rate={} rejected because max forward jump of {} sec exceeds i32::MAX",
                clock_rate, MAX_FORWARD_TIME_JUMP_SECS))?;
        Ok(Timeline {
            timestamp: i64::from(start.unwrap_or(0)),
            start,
            clock_rate,
            max_forward_jump,
        })
    }

    /// Advances to the given (wrapping) RTP timestamp, creating a monotonically
    /// increasing [Timestamp]. Errors on excessive or backward time jumps.
    pub(super) fn advance_to(&mut self, rtp_timestamp: u32) -> Result<Timestamp, Error> {
        let (timestamp, delta) = self.ts_and_delta(rtp_timestamp)?;
        if !(0..self.max_forward_jump).contains(&delta) {
            bail!("Timestamp jumped {} ({:.03} sec) from {} to {}; \
                   policy is to allow 0..{} sec only",
                  delta, (delta as f64) / f64::from(self.clock_rate.get()), self.timestamp,
                  timestamp, MAX_FORWARD_TIME_JUMP_SECS);
        }
        self.timestamp = timestamp.timestamp;
        Ok(timestamp)
    }

    /// Places `rtp_timestamp` on the timeline without advancing the timeline
    /// or applying time jump policy. Will set the NPT epoch if unset.
    ///
    /// This is useful for RTP timestamps in RTCP packets. They commonly refer
    /// to time slightly before the most timestamp of the matching RTP stream.
    pub(super) fn place(&mut self, rtp_timestamp: u32) -> Result<Timestamp, Error> {
        Ok(self.ts_and_delta(rtp_timestamp)?.0)
    }

    fn ts_and_delta(&mut self, rtp_timestamp: u32) -> Result<(Timestamp, i32), Error> {
        let start = match self.start {
            None => {
                self.start = Some(rtp_timestamp);
                self.timestamp = i64::from(rtp_timestamp);
                rtp_timestamp
            },
            Some(start) => start,
        };
        let delta = (rtp_timestamp as i32).wrapping_sub(self.timestamp as i32);
        let timestamp = self.timestamp.checked_add(i64::from(delta)).ok_or_else(|| {
                // This probably won't happen even with a hostile server. It'd
                // take ~2^31 packets (~ 4 billion) to advance the time this far
                // forward or backward even with no limits on time jump per
                // packet.
                format_err!("timestamp {} + delta {} won't fit in i64!",
                            self.timestamp, delta)
            })?;

        // Also error in similarly-unlikely NPT underflow.
        if timestamp.checked_sub(i64::from(start)).is_none() {
            bail!("timestamp {} + delta {} - start {} underflows i64!",
                  self.timestamp, delta, start);
        }
        Ok((Timestamp {
            timestamp,
            clock_rate: self.clock_rate,
            start,
        }, delta))
    }
}

#[cfg(test)]
mod tests {
    use super::Timeline;

    #[test]
    fn timeline() {
        // Don't allow crazy clock rates that will get us into trouble.
        Timeline::new(Some(0), 0).unwrap_err();
        Timeline::new(Some(0), u32::MAX).unwrap_err();

        // Don't allow excessive forward jumps.
        let mut t = Timeline::new(Some(100), 90_000).unwrap();
        t.advance_to(100 + (super::MAX_FORWARD_TIME_JUMP_SECS * 90_000) + 1).unwrap_err();

        // Or any backward jump.
        let mut t = Timeline::new(Some(100), 90_000).unwrap();
        t.advance_to(99).unwrap_err();

        // ...but do allow backward RTP timestamps in RTP.
        let mut t = Timeline::new(Some(100), 90_000).unwrap();
        assert_eq!(t.place(99).unwrap().elapsed(), -1);
        assert_eq!(t.advance_to(101).unwrap().elapsed(), 1);

        // Normal usage.
        let mut t = Timeline::new(Some(42), 90_000).unwrap();
        assert_eq!(t.advance_to(83).unwrap().elapsed(), 83 - 42);
        assert_eq!(t.advance_to(453).unwrap().elapsed(), 453 - 42);

        // Wraparound is normal too.
        let mut t = Timeline::new(Some(u32::MAX), 90_000).unwrap();
        assert_eq!(t.advance_to(5).unwrap().elapsed(), 5 + 1);

        // No initial rtptime.
        let mut t = Timeline::new(None, 90_000).unwrap();
        assert_eq!(t.advance_to(218250000).unwrap().elapsed(), 0);
    }

    #[test]
    fn cast() {
        let a = 0x1FFFF_FFFFi64;
        let b = 0x10000_0000i64;
        assert_eq!(a as i32, -1);
        assert_eq!(b as i32, 0);
    }
}
