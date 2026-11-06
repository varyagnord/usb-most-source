#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
mkdir -p build/tests-native
cc -std=c11 -Wall -Wextra -Werror -g -fsanitize=address,undefined -fno-omit-frame-pointer -pthread tests/native/test_engine.c -o build/tests-native/test_engine
ASAN_OPTIONS=detect_leaks=1 build/tests-native/test_engine
