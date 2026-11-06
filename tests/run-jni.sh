#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --locked --release -p usb-most-native
USB_MOST_JNI_LIB="${CARGO_TARGET_DIR:-build/rust-target}/release"
bash tests/run-java.sh
javac -cp build/tests-java -d build/tests-java app/src/main/java/org/cgutman/usbip/jni/NativeUsb.java tests/java/NativeJniRegressionTests.java
bash tests/java-isolated.sh -Xcheck:jni --add-opens java.base/java.io=ALL-UNNAMED -Djava.library.path="$USB_MOST_JNI_LIB" -cp build/tests-java NativeJniRegressionTests

javac -cp build/tests-java -d build/tests-java app/src/main/java/org/cgutman/usbip/jni/NativePolicy.java tests/java/PolicyJniRegressionTests.java
bash tests/java-isolated.sh -Xcheck:jni -Djava.library.path="$USB_MOST_JNI_LIB" -cp build/tests-java PolicyJniRegressionTests

cargo run --locked -p usb-most-connection --example pairing_qr_fixture -- build/e2e/qr-fixture
javac -cp build/tests-java -d build/tests-java app/src/main/java/org/cgutman/usbip/jni/NativeConnection.java tests/java/ConnectionJniRegressionTests.java
bash tests/java-isolated.sh -Xcheck:jni -Djava.library.path="$USB_MOST_JNI_LIB" -cp build/tests-java ConnectionJniRegressionTests build/e2e/qr-fixture

javac -cp build/tests-java -d build/tests-java tests/java/ComputersJniRegressionTests.java
bash tests/java-isolated.sh -Xcheck:jni -Djava.library.path="$USB_MOST_JNI_LIB" -cp build/tests-java ComputersJniRegressionTests

javac -cp build/tests-java -d build/tests-java app/src/main/java/org/cgutman/usbip/jni/NativeManagement.java tests/java/ManagementJniRegressionTests.java
bash tests/java-isolated.sh -Xcheck:jni -Djava.library.path="$USB_MOST_JNI_LIB" -cp build/tests-java ManagementJniRegressionTests

javac -cp build/tests-java -d build/tests-java tests/java/ManagedServerRegressionTests.java
bash tests/java-isolated.sh -Xcheck:jni -Djava.library.path="$USB_MOST_JNI_LIB" -cp build/tests-java ManagedServerRegressionTests
