# Not pinned by digest like the other images: registry.opensuse.org keeps
# only the newest Tumbleweed snapshots, so a pinned digest stops resolving
# within days, and a release built later from its tag would fail.
FROM registry.opensuse.org/opensuse/tumbleweed:latest
RUN zypper --non-interactive install --no-recommends curl git gzip rpm-build rpmlint tar && zypper clean --all
