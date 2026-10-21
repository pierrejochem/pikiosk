# Build environment for pikiosk. Works natively on Apple Silicon (arm64)
# and on x86_64; Buildroot compiles its own cross toolchain either way.
FROM debian:bookworm

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash bc binutils build-essential bzip2 ca-certificates cpio file git \
        libncurses-dev locales patch perl python3 rsync unzip wget xz-utils \
    && rm -rf /var/lib/apt/lists/* \
    && sed -i 's/# en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen

ENV LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8

RUN useradd -m -u 1000 builder && mkdir -p /work /home/builder/.buildroot-ccache \
    && chown builder:builder /work /home/builder/.buildroot-ccache
USER builder
WORKDIR /work
# a named volume mounted here inherits builder ownership on first use and
# keeps sources and build output on a case-sensitive filesystem; build.sh also
# mounts a volume on ~/.buildroot-ccache, so compiler output survives a clean
VOLUME /work
