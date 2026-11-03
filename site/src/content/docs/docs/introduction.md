---
title: Introduction
description: What Vitrine is and how it differs from a classic desktop theme engine.
---

Vitrine is a desktop widget engine for Windows 10 and 11. It draws widgets **beneath your desktop
icons**, on the same layer as the wallpaper, and keeps them **interactive**.

:::note
Vitrine 2 is under active development. This documentation describes the design being built and will
grow with each milestone.
:::

## Widgets on a grid

Instead of a single full-screen "theme", your desktop becomes a grid:

- every **widget** occupies a rectangle of grid cells and can be moved and resized in **edit mode**;
- you can add several **instances** of the same widget, each with its own settings;
- every monitor has its own grid, and layouts adapt to DPI and resolution changes.

## Native, not a browser

Vitrine 1 rendered themes with WebView2, a full Chromium engine. Vitrine 2 has its own renderer:

- widget code runs in **QuickJS**, a small embeddable JavaScript engine, with one isolated runtime per
  widget;
- the widget's UI tree is laid out with **Taffy** (flexbox and grid) and painted with **Direct2D** and
  **DirectWrite**;
- frames are drawn only when something changes, and nothing runs when the desktop is hidden.

## Widgets are packages

A widget is a `.vwidget` file: a manifest, a JavaScript bundle written with `@vitrine/sdk` (JSX and
SolidJS), assets such as fonts and images, and an optional settings schema. Each widget declares the
permissions it needs, for example CPU metrics or a specific weather API host.
