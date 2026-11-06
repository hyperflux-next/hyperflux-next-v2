FROM docker.io/library/fedora:45@sha256:a7d72ba2745b100a3351087472e5ee7b2450af7cf331e7673007e3817612839c
RUN dnf install -y --setopt=install_weak_deps=False curl git rpm-build rpmlint && dnf clean all
