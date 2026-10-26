FROM docker.io/library/archlinux:base-devel@sha256:3cbf69d2d5958d6a294f3722c5668e6187c4df8e00fa27f72352f0bf83e495e1
RUN pacman -Syu --noconfirm --needed git namcap && pacman -Scc --noconfirm \
    && useradd --create-home builder
