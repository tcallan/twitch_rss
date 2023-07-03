FROM rust:1 AS build
WORKDIR /usr/src

# create a dummy project and build dependencies
RUN USER=root cargo new twitch_rss
WORKDIR /usr/src/twitch_rss
COPY Cargo.toml Cargo.lock ./
RUN cargo build --release

# remove artifacts from building dependencies
RUN rm src/*.rs
RUN rm /usr/src/twitch_rss/target/release/deps/twitch_rss*

# build the application
COPY src ./src
RUN cargo build --release

FROM debian:bullseye-slim
RUN apt update && apt install -y libssl-dev ca-certificates
COPY --from=build /usr/src/twitch_rss/target/release/twitch_rss .

ENV ROCKET_ADDRESS=0.0.0.0
# ENV TWITCH_CLIENT_ID
# ENV TWITCH_CLIENT_SECRET

CMD ROCKET_PORT=$PORT ./twitch_rss