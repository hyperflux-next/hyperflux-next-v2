FROM docker.io/library/debian:forky@sha256:8ba3df177e65d13cb4392a490db1c48cb7f85461b5769f60eb37725221b8b203
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl dpkg-dev git lintian && rm -rf /var/lib/apt/lists/*
