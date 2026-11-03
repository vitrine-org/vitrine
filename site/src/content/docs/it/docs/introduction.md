---
title: Introduzione
description: Cos'è Vitrine e in cosa si differenzia da un classico motore di temi per il desktop.
---

Vitrine è un motore di widget per il desktop di Windows 10 e 11. Disegna i widget **sotto le icone del
desktop**, sullo stesso livello dello sfondo, e li mantiene **interattivi**.

:::note
Vitrine 2 è in pieno sviluppo. Questa documentazione descrive il progetto in costruzione e crescerà a
ogni milestone.
:::

## Widget su una griglia

Invece di un unico "tema" a schermo intero, il desktop diventa una griglia:

- ogni **widget** occupa un rettangolo di celle e si sposta e ridimensiona in **modalità modifica**;
- puoi aggiungere più **istanze** dello stesso widget, ognuna con le sue impostazioni;
- ogni monitor ha la sua griglia, e il layout si adatta ai cambi di DPI e risoluzione.

## Nativo, non un browser

Vitrine 1 disegnava i temi con WebView2, un motore Chromium completo. Vitrine 2 ha un renderer proprio:

- il codice dei widget gira in **QuickJS**, un motore JavaScript piccolo e integrabile, con un runtime
  isolato per ogni widget;
- l'albero dell'interfaccia viene impaginato con **Taffy** (flexbox e grid) e disegnato con
  **Direct2D** e **DirectWrite**;
- si disegna un frame solo quando qualcosa cambia, e non gira nulla quando il desktop è nascosto.

## I widget sono pacchetti

Un widget è un file `.vwidget`: un manifest, un bundle JavaScript scritto con `@vitrine/sdk` (JSX e
SolidJS), risorse come font e immagini e uno schema di impostazioni facoltativo. Ogni widget dichiara i
permessi di cui ha bisogno, per esempio le metriche della CPU o un host specifico per il meteo.
