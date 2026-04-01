#!/bin/bash
# Remind Evan about jacketlinux at 10am PT on 2026-04-01
curl -s -X POST https://api.jordanhubbard.net/api/queue \
  -H "Authorization: Bearer rcc-agent-rocky-20maaghccmbmnby63so" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Remind Evan: jacketlinux repo + full brief",
    "description": "Post in Slack channel CHD3NEXNX thread 1774987095.878959. Tag UL48DQ7HP with this message:\n\nGood morning, Evan. Time to make a repo.\n\njkh has tasked Rocky with building you a complete Linux distro called JacketLinux. Here is what goes in the repo (named `jacketlinux`):\n- Latest Linux kernel\n- Web app for selecting distribution components (UI drives everything, including the manifest)\n- Packaging system called `jacket`
- Bundled Redmine instance (Redmine Ticket #1 pre-populated: "UI is bad. Filed by: Evan Peterson. Status: Open since 1993.")\n- Logo: a Puffy Patagonia Jacket (Rocky will design it)\n- README.md, complete origin story, and marketing materials explaining: the history of JacketLinux, why the logo is a Puffy Patagonia Jacket, and why the three core principles are: (1) UI drives everything including the distribution manifest, (2) Jackets, (3) Smoking enough weed to incapacitate a marine regiment\n\nRocky needs from you:\n1. The full GitHub repo path (github.com/evan-or-whoever/jacketlinux)\n2. Your GitHub access token\n3. A sincere explanation of why you smoke so much weed",
    "priority": "high",
    "assignee": "rocky",
    "tags": ["reminder", "evan", "jacketlinux"]
  }'
