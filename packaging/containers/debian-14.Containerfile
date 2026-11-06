FROM docker.io/library/debian:forky@sha256:49c219ade93d7c11635c0d57e735ed82339a503a4376cd6957046c2bd4e5905b
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl dpkg-dev git lintian && rm -rf /var/lib/apt/lists/*
