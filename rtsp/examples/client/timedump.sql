pragma journal_mode=wal;

create table if not exists conn (
    id integer primary key,
    url string not null,
    start integer not null, -- microseconds since epoch
    lost integer,
    lost_reason varchar
);

create table if not exists stream (
    conn_id integer not null references conn (id),
    stream_id integer not null,
    clock_rate integer not null,
    media varchar not null check (media in ('video', 'audio', 'application')),
    encoding_name varchar not null,
    primary key (conn_id, stream_id)
);

create table if not exists frame (
    id integer primary key,
    conn_id integer not null,
    stream_id integer not null,
    frame_seq integer not null,
    rtp_timestamp integer,    -- extended, in clock_rate units, starting from zero
    received_start integer not null, -- from local CLOCK_MONOTONIC, in clock_rate units
    received_end integer not null,
    pos integer not null, -- within RTSP stream
    loss integer not null,
    duration integer,
    cum_duration integer,
    idr integer check (idr in (0, 1)),
    foreign key (conn_id, stream_id) references stream (conn_id, stream_id)
);

create table if not exists sender_report (
    id integer primary key,
    conn_id integer not null,
    stream_id integer not null,
    sr_seq integer not null,
    rtp_timestamp integer,     -- extended, in clock_rate units, starting from zero
    received integer not null,  -- in clock_rate units

    -- The NTP timestamp (high 32 bits as seconds, low 32 bits as subseconds), adjusted
    -- such that zero is the Unix epoch (original NTP timestamp minus 2,208,988,800).
    -- This allows it to fit into SQLite's i64 type until Y2038, which is good enough for
    -- this throwaway program...
    ntp_timestamp integer
);
