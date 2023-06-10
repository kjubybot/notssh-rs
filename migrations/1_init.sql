CREATE TABLE IF NOT EXISTS clients (
    id varchar primary key,
    address varchar,
    connected boolean NOT NULL,
    last_online timestamp with time zone NOT NULL
);
CREATE TABLE IF NOT EXISTS actions (
    id varchar primary key,
    client_id varchar NOT NULL,
    created_at timestamp with time zone NOT NULL,
    started_at timestamp with time zone,
    timeout bigint,
    command smallint NOT NULL,
    state smallint NOT NULL,
    error bytea,
    result bytea
);
CREATE TABLE IF NOT EXISTS ping (
    id varchar primary key,
    data varchar NOT NULL
);
CREATE TABLE IF NOT EXISTS purge (
    id varchar primary key
);
CREATE TABLE IF NOT EXISTS shell (
    id varchar primary key,
    cmd varchar NOT NULL,
    args varchar[] NOT NULL,
    stdin bytea NOT NULL
);

