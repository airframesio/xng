#!/bin/bash

DEB_ARCH=$(dpkg --print-architecture)
DEB_BUILD_ROOT=$(pwd)/build

DEB_PKG_VERSION=$(grep Version $(pwd)/packaging/control | awk '{print $2}')
DEB_PKG_NAME=xng-${DEB_PKG_VERSION}

mkdir -p ${DEB_BUILD_ROOT}/${DEB_PKG_NAME}/DEBIAN
mkdir -p ${DEB_BUILD_ROOT}/${DEB_PKG_NAME}/usr/bin

cp $(pwd)/target/release/${DEB_PKG_NAME} ${DEB_BUILD_ROOT}/${DEB_PKG_NAME}/usr/bin
sed -e s/CURRENT_ARCH/${DEB_ARCH}/ $(pwd)/packaging/control > ${DEB_BUILD_ROOT}/${DEB_PKG_NAME}/DEBIAN/control

pushd ${DEB_BUILD_ROOT} && \
  dpkg-deb --build ${DEB_PKG_NAME} && \
  popd

