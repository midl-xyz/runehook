FROM rust:1.81.0-bullseye AS build
WORKDIR /app
COPY . .
RUN cargo runehook-install

FROM alpine:latest
RUN apk add --no-cache ca-certificates
WORKDIR /app
COPY --from=build /app/target/release/runehook /bin/runehook
CMD ["runehook"]