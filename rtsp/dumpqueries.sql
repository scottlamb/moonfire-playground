.headers on
.mode column

.print Connection losses
select
  datetime(lost / 1.e6, 'unixepoch', 'localtime') as lost,
  url,
  lost_reason
from
  conn
where
  lost_reason is not null
order by lost;

-- When there's packet loss, how long was it before the previous frame?
-- XXX: this would be more straightforward using SQL window functions.
.print
.print Packet loss stats (non-initial only)
select
  datetime(conn.start / 1.e6 + frame.received_start / cast(stream.clock_rate as real), 'unixepoch', 'localtime') as tm,
  frame.loss,
  cast((frame.received_start - prev_frame.received_start) as real) / stream.clock_rate as delay_sec,
  cast((frame.rtp_timestamp - frame.received_start) as real) / stream.clock_rate as ahead_sec,
  stream.media,
  conn.url,
  frame.idr
from
  frame
  join frame prev_frame on (
    frame.conn_id = prev_frame.conn_id and
    frame.stream_id = prev_frame.stream_id and
    frame.frame_seq = prev_frame.frame_seq + 1)
  join stream on (frame.conn_id = stream.conn_id and frame.stream_id = stream.stream_id)
  join conn on (frame.conn_id = conn.id)
where
  frame.loss > 0
order by tm;

-- How far ahead and behind real time do we get?
.print
.print Ahead and behind stats
select
  conn.url,
  stream.media,
  max(frame.rtp_timestamp - first_frame.rtp_timestamp - frame.received_start + first_frame.received_start) as max_ahead,
  min(frame.rtp_timestamp - first_frame.rtp_timestamp - frame.received_start + first_frame.received_start) as min_ahead
from
  frame
  join frame first_frame on (
    frame.conn_id = first_frame.conn_id and
    frame.stream_id = first_frame.stream_id and
    first_frame.frame_seq = 0
  )
  join conn on (frame.conn_id = conn.id)
  join stream on (
    frame.conn_id = stream.conn_id and
    frame.stream_id = stream.stream_id and
    stream.media != 'application')
group by 1, 2
order by 1, 2;

.print
.print Duration-relative
select
  conn.url,
  stream.media,
  max(frame.rtp_timestamp - first_frame.rtp_timestamp - frame.cum_duration) as max_ahead,
  min(frame.rtp_timestamp - first_frame.rtp_timestamp - frame.cum_duration) as min_ahead
from
  frame
  join frame first_frame on (
    frame.conn_id = first_frame.conn_id and
    frame.stream_id = first_frame.stream_id and
    first_frame.frame_seq = 0
  )
  join conn on (frame.conn_id = conn.id)
  join stream on (
    frame.conn_id = stream.conn_id and
    frame.stream_id = stream.stream_id and
    stream.media != 'application')
group by 1, 2
order by 1, 2;

.print
.print Last frame in stream
select
  conn.url,
  stream.media,
  last_frame.rtp_timestamp / cast(stream.clock_rate as real) as rtp,
  (last_frame.rtp_timestamp - last_frame.cum_duration) / cast(stream.clock_rate as real) as rtp_minus_dur,
  (last_frame.rtp_timestamp - last_frame.received_start) / cast(stream.clock_rate as real) as rtp_minus_recv,
  stream.clock_rate
from (
  select
    row_number() over (partition by conn_id, stream_id order by id desc) as row,
    conn_id,
    stream_id,
    rtp_timestamp,
    cum_duration,
    received_start
  from
    frame
   where cum_duration is not null) last_frame
  join stream on (last_frame.conn_id = stream.conn_id and last_frame.stream_id = stream.stream_id)
  join conn on (last_frame.conn_id = conn.id)
where
  row = 1
 order by 1, 2;

select
  conn.url,
  conn.id,
  stream.media,
  min(received_start) / cast(clock_rate as real) as first_recv,
  max(received_start) / cast(clock_rate as real) as last_recv,
  count(*) * cast(clock_rate as real) / (max(received_start) - min(received_start)) as rate,
  count(*) as cnt
from
  frame
  join stream on (frame.conn_id = stream.conn_id and frame.stream_id = stream.stream_id)
  join conn on (frame.conn_id = conn.id)
group by 1, 2, 3
order by 1, 2, 3;
