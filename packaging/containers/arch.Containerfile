FROM docker.io/library/archlinux:base-devel@sha256:624bdc7fc6615155d67bc34cf7146c0c9fd0e3c92943977da63a630e4ca49c30
RUN pacman -Syu --noconfirm --needed git namcap && pacman -Scc --noconfirm \
    && useradd --create-home builder
