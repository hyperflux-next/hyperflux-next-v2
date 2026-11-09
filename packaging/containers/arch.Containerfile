FROM docker.io/library/archlinux:base-devel@sha256:2de80105abb49f813a85393c6758a4334deb6bcec4a2411bd2f059c0bee6ee4c
RUN pacman -Syu --noconfirm --needed git namcap && pacman -Scc --noconfirm \
    && useradd --create-home builder
