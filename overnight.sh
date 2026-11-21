#!/bin/bash
cd ~/Desktop/Projects/shopping-sessions
mkdir -p logs
for i in $(seq 1 16); do
  start=$(date +%s)
  claude --chrome --dangerously-skip-permissions -p "$(cat overnight.md)" \
    >> logs/cycle-$i.log 2>&1
  # Make sure each cycle takes at least 31 minutes (max 2 submits/hour)
  elapsed=$(( $(date +%s) - start ))
  [ $elapsed -lt 1860 ] && sleep $(( 1860 - elapsed ))
done
