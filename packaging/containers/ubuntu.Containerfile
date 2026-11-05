FROM docker.io/library/ubuntu:26.10@sha256:262df65c0ec6bf8d9dcc87454f4330a05fae4b97825f45854af3b136666584c2
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl dpkg-dev git lintian && rm -rf /var/lib/apt/lists/*
