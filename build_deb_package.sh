#!/bin/bash

DEB_ARCH=$(dpkg --print-architecture)
DEB_BUILD_ROOT=$(pwd)/build

mkdir -p ${DEB_BUILD_ROOT}/xng/DEBIAN
mkdir -p ${DEB_BUILD_ROOT}/xng/usr/bin

cp $(pwd)/target/release/xng ${DEB_BUILD_ROOT}/xng/usr/bin
sed -e s/CURRENT_ARCH/${DEB_ARCH}/ packaging/control > ${DEB_BUILD_ROOT}/xng/DEBIAN/control

pushd ${DEB_BUILD_ROOT} && \
  dpkg-deb --build xng && \
  popd
