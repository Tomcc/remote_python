#!/bin/bash

Killall remote_python

if cargo build --release; then
	cd test
	#start server
	../target/release/remote_python --server localhost &

	sleep 0.1

	../target/release/remote_python "localhost" "test.py"

	exit 0
fi
exit 1


