#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
mkdir -p build/tests-java
find app/src/main/java/org/cgutman/usbip/server app/src/main/java/org/cgutman/usbip/core app/src/main/java/org/cgutman/usbip/utils -name '*.java' > build/tests-java/sources
javac -Xlint:all -d build/tests-java @build/tests-java/sources tests/java/RegressionTests.java
bash tests/java-isolated.sh -ea -cp build/tests-java RegressionTests
javac -Xlint:all -d build/tests-java app/src/main/java/org/cgutman/usbip/connection/ConnectionObservation.java tests/java/ConnectionObservationTests.java
bash tests/java-isolated.sh -ea -cp build/tests-java org.cgutman.usbip.connection.ConnectionObservationTests
