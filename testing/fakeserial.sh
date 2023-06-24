#!/bin/bash
socat -u pty,link=master.pty,raw,echo=0 pty,link=fakeserial.pty,raw,echo=0 &
while :
do
  while IFS= read -r line; do
    echo "$line"
    echo "$line">master.pty
    sleep 0.2
  done < going-north-10kph.log
done
