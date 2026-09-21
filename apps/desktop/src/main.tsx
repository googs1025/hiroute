import React from 'react';
import { createRoot } from 'react-dom/client';
import { DesktopApp } from './product/DesktopApp';
import { RenderFailureBoundary } from './ui/RenderFailureBoundary';
import './occami/styles.css';

createRoot(document.getElementById('root')!).render(
  <RenderFailureBoundary>
    <DesktopApp />
  </RenderFailureBoundary>,
);
