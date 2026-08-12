#!/usr/bin/env bash

if [ $# -ne 1 ]
then
    echo "Usage: $0 <file>"
    exit
fi
   
md5sum $1 |awk '{print $1}' |sed 's/../0x&, /g;s/, $//'
