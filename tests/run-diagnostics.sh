#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
bash tests/run-java.sh
javac -Xlint:all -cp build/tests-java -d build/tests-java \
  tests/java/DiagnosticBackendTests.java tests/java/DiagnosticProtocolTests.java tests/java/WanProtocolTests.java
bash tests/java-isolated.sh -ea -cp build/tests-java DiagnosticBackendTests
bash tests/java-isolated.sh -ea -cp build/tests-java DiagnosticProtocolTests
bash tests/java-isolated.sh -ea -cp build/tests-java WanProtocolTests
CARGO_TARGET_DIR="$PWD/build/rust-diagnostics-host" cargo build --locked --release --features diagnostics -j 2
javac -Xlint:all -cp build/tests-java -d build/tests-java \
  app/src/main/java/org/cgutman/usbip/jni/NativeUsb.java \
  app/src/main/java/org/cgutman/usbip/service/DiagnosticSelfTest.java \
  tests/java/DiagnosticJniRegressionTests.java
bash tests/java-isolated.sh -ea -Xcheck:jni --add-opens java.base/java.io=ALL-UNNAMED \
  -Djava.library.path=build/rust-diagnostics-host/release \
  -cp build/tests-java org.cgutman.usbip.service.DiagnosticJniRegressionTests
