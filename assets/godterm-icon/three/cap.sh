#!/bin/bash
# usage: cap.sh <name>  -> renders three/<name>.html to three/renders/<name>-2048.png
cd "$(dirname "$0")"; mkdir -p renders
NODE_PATH=${NODE_PATH:?set NODE_PATH to a node_modules with puppeteer} node capture.cjs "$1.html${2:+?$2}" "renders/$1${2:+-$2}-2048.png" 2>&1 | grep -v '^\[page\] ready' | grep -v zoxide
