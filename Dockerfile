FROM rust:1 AS build
WORKDIR /usr/src

# create a dummy project and build dependencies
RUN USER=root cargo new twitch_rss
WORKDIR /usr/src/twitch_rss
COPY Cargo.toml Cargo.lock ./
RUN cargo build --release

# build the application
COPY src ./src
RUN cargo install --path .

FROM debian:buster-slim
RUN apt update && apt install -y libssl-dev ca-certificates
COPY --from=build /usr/local/cargo/bin/twitch_rss .

ENV ROCKET_ADDRESS=0.0.0.0
ENV ROCKET_PORT=$PORT
ENV TWITCH_CLIENT_ID
ENV TWITCH_CLIENT_SECRET

CMD ["./twitch_rss"]