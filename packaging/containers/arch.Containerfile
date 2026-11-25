FROM docker.io/library/archlinux:base-devel@sha256:fa2d4ce3310ddd90a4208e5168be8ad367da5129af1245f38fd2d803bcbe3239
RUN pacman -Syu --noconfirm --needed git namcap && pacman -Scc --noconfirm \
    && useradd --create-home builder
