Zil
===

Zil is a fork of [Zed v1.6.3](https://github.com/zed-industries/zed/tree/v1.6.3).

Features
--------

The biggest feature of Zil is that it doesn't have many features! How many features? _**Zil**ch!_ 🤏

It is a bare-bones text editor, and doesn't get in your way.

I have **removed** the vast majority of features from Zed, including AI Agents, LSPs, auto-updates,
edit completions, inlay hints, projects, git diff viewing, extensions, random downloading of binary
files, collaboration, formatting, code actions, and more! I have deleted over 1 _million_ lines of
code 🎉!

I am a simple man. I just want to sit and type. Sometimes I use multiple cursors. That's about it.

I did add a few features, believe it or not:

1. Stacked tabs in the tab bar (more convenient when lots of tabs open -- no scrolling!)
2. Simple plugin system for syntax highlighting
3. Simplified auto-indent system to be more predictable
4. Simplified tabs/spaces system to separate it from language config
5. Assorted behavior tweaks, mostly copying Sublime Text 3's nuances

Why Fork?
---------

Zed is a nice editor, but I really dislike the constant updates and features that I need to disable.

There are two other forks I'm aware of:

1. [Zedless](https://github.com/zedless-editor/zedless)
2. [Gram](https://gram.liten.app/)

Both of these seem great, but they still have too many features.

My philosophy when it comes to tools is that they should do one thing well. I don't need a terminal
embedded in my editor -- I have one already. I don't need to browse folders, I have Finder. I don't
need to have AI, I can open one in a browser if I want one. I don't need collaboration tools, I have
Google Meet.

This way, my entire desktop is my "IDE"! Easy!

Still Left To Do
----------------

Lots! Everywhere I look, there is something that can be ripped out.

Major changes still needed:

1. Rip out LSPs entirely. They don't run, but a lot of code is still there.
2. Rip out client/remote entirely.
3. Rip out git/diffs entirely.
4. Rework themes and add to new plugin system.
5. Clean up _lots_ of dead code that Rust misses.
6. Rename symbols/paths/etc from "Zed" to "Zil".
7. Build binaries for Mac/Windows/Linux when ready.
