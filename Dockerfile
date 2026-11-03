# A container image built from an already-packaged Linux server build.
#
#   scripts/package.sh --kind server --os linux --arch x64 --no-archive
#   docker build --build-arg PACKAGE_DIR=release/<version>/linux/x64/server -t romexplorer:latest .
#
# PACKAGE_DIR has no default: the staged path carries the version, and a default naming one
# version would go stale at the next bump and copy in an old package without complaint.
#   docker run -d --name romexplorer -p 7337:7337 -v romexplorer-data:/var/lib/romexplorer romexplorer:latest
#
# Deliberately NOT a multi-stage image that compiles from source. The native dependencies
# alone take the better part of an hour (scripts/build_native_deps.sh), and the package this
# copies in is the same artifact the tarball and the installer ship -- so the image is tested
# by everything that tests those, and a rebuild is seconds rather than an afternoon. Build
# the package first; this turns it into an image.
#
# Base image: debian:12-slim has glibc 2.36, and .github/workflows/release-packages.yml builds
# on ubuntu-22.04 (glibc 2.35), so a package built by CI runs here. Building the package on a
# newer host than this base is the one way to get a "version `GLIBC_2.xx' not found" on start.

ARG PACKAGE_DIR

FROM debian:12-slim

# mariadbd and the bundled libraries are all dlopen()ed from inside the package, so the only
# things needed from the distribution are a CA bundle for outbound provider calls and
# libstdc++/libgcc, which the vendored C++ libraries link against dynamically.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates libstdc++6 libgcc-s1 \
    && rm -rf /var/lib/apt/lists/*

# A fixed uid/gid, so a bind-mounted host directory has a predictable owner. mariadbd refuses
# to run as root and is passed no --user, so this is required, not hardening for its own sake.
RUN groupadd --system --gid 10001 romexplorer \
    && useradd --system --uid 10001 --gid romexplorer --home-dir /var/lib/romexplorer \
        --shell /usr/sbin/nologin romexplorer

ARG PACKAGE_DIR
COPY ${PACKAGE_DIR} /opt/romexplorer

# Everything written at run time goes to one volume, the same redirection the AppImage uses
# (see AppPaths::assets_root): without it the database, the downloaded DATs and the media
# would land in the image layer, which is neither persistent nor where gigabytes belong.
ENV ROMEXPLORER_APP_ROOT=/opt/romexplorer \
    ROMEXPLORER_DATA_DIR=/var/lib/romexplorer/data \
    ROMEXPLORER_ASSETS_DIR=/var/lib/romexplorer/assets \
    ROMEXPLORER_CACHE_DIR=/var/lib/romexplorer/cache \
    ROMEXPLORER_SERVICE_MANAGED=1

RUN mkdir -p /var/lib/romexplorer/data /var/lib/romexplorer/assets /var/lib/romexplorer/cache \
    && chown -R romexplorer:romexplorer /var/lib/romexplorer /opt/romexplorer \
    && chmod +x /opt/romexplorer/romexplorer-server-start

VOLUME ["/var/lib/romexplorer"]
EXPOSE 7337

USER romexplorer
WORKDIR /opt/romexplorer

# 0.0.0.0, because 127.0.0.1 inside a container is reachable from nothing. The server still
# serves TLS with a certificate it makes on first start and still redirects plain HTTP from
# anywhere but itself, so this is not the same as making it unauthenticated.
#
# romexplorer-server-start, not romexplorer_server: it adds --init-from-config on a first
# start if data/setup.env is mounted in, which is how a container gets its accounts without
# an interactive step. ROMEXPLORER_SERVICE_MANAGED=1 means `docker stop`'s SIGTERM shuts the
# database down cleanly and the console's restart exits 75 for the restart policy to act on,
# rather than forking a second server inside the container.
ENTRYPOINT ["/opt/romexplorer/romexplorer-server-start"]
CMD ["--bind", "0.0.0.0", "--port", "7337"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=180s --retries=3 \
    CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/7337' || exit 1
