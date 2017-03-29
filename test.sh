#!/bin/bash

Killall remote_python

if cargo install --force; then
	#start server
	remote_python &

	remote_python --client "localhost:55455" "test.py"

	exit 0
fi
exit 1


