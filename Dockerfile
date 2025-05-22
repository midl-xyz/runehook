FROM rust:1.81.0-bullseye AS build
WORKDIR /app
COPY . .
RUN cargo runehook-install

FROM debian:bullseye-slim
RUN apt update && apt install -y ca-certificates
WORKDIR /app
COPY --from=build /app/target/release/runehook /bin/runehook
CMD ["runehook"]