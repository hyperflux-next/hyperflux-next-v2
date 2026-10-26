FROM docker.io/library/ubuntu:26.10@sha256:93e29beff5257b2cdf1c024d831f6d2997e53655a653ff76a811fc9e6564a719
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl dpkg-dev git lintian && rm -rf /var/lib/apt/lists/*
