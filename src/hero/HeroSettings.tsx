import { useEffect, useState } from 'react';
import type { CSSProperties } from 'react';
import { ADES, type Ade } from '../ade';
import { normalizeParadevSettings, paradevPatternDefaults } from './paradev-pixels';

const STORAGE_KEY = 'instabox.hero-preview.v1';
export const heroSettingsDefaults = {
  backgroundOpacity: 100, cubeOpacity: 100, fadeStart: 56, fadeEnd: 86, cubeY: 64,
  orcaSpeed: 100, orcaScale: 130, orcaOpacity: 20, orcaY: 25, orcaPaused: 0,
  paradevLightOpacity: 60, paradevLightWidth: 100, paradevLightReach: 200,
  ...paradevPatternDefaults,
};
type Settings = typeof heroSettingsDefaults;
type SettingsByAde = Record<Ade, Settings>;
const defaultsFor = (ade: Ade): Settings => ({
  ...heroSettingsDefaults,
  ...(ade === 'paradev' ? { backgroundOpacity: 48, cubeOpacity: 100, fadeStart: 61, fadeEnd: 100, pixelX: 68, pixelY: 22 } : {}),
  ...(ade === 'cmux' ? { backgroundOpacity: 40, fadeStart: 0, fadeEnd: 100 } : {}),
});
type Control = { key: keyof Settings; label: string; min: number; max: number; unit?: string };
const lightControls: Control[] = [
  { key: 'paradevLightOpacity', label: 'Light brightness', min: 0, max: 100 },
  { key: 'paradevLightWidth', label: 'Light width', min: 25, max: 200 },
  { key: 'paradevLightReach', label: 'Light downward reach', min: 25, max: 200 },
];
const patternControls: Control[] = [
  { key: 'pixelX', label: 'Pattern horizontal position', min: -100, max: 100 },
  { key: 'pixelY', label: 'Pattern vertical position', min: -100, max: 100 },
  { key: 'pixelOpacity', label: 'Pattern opacity', min: 0, max: 100 },
  { key: 'pixelSize', label: 'Pixel size', min: 1, max: 5, unit: 'px' },
  { key: 'pixelDensity', label: 'Pattern density', min: 0, max: 200 },
  { key: 'pixelSpeed', label: 'Animation speed', min: 0, max: 300 },
];
const orcaControls: Control[] = [
  { key: 'orcaSpeed', label: 'Orca speed', min: 25, max: 300 },
  { key: 'orcaScale', label: 'Orca size', min: 10, max: 300 },
  { key: 'orcaOpacity', label: 'Orca visibility', min: 0, max: 100 },
  { key: 'orcaY', label: 'Orca vertical position', min: 0, max: 100 },
];
const controls: Control[] = [
  { key: 'backgroundOpacity', label: 'Background opacity', min: 0, max: 100 },
  { key: 'cubeOpacity', label: 'Cube opacity', min: 0, max: 100 },
  { key: 'fadeStart', label: 'Fade starts', min: 0, max: 95 },
  { key: 'fadeEnd', label: 'Fully faded', min: 5, max: 100 },
  { key: 'cubeY', label: 'Cube vertical position', min: 40, max: 80 },
];

function readSettings(): SettingsByAde {
  let stored: Partial<Record<Ade, Partial<Settings>>> & { paradevOpacityVersion?: number } = {};
  if (import.meta.env.DEV) {
    try { stored = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}') ?? {}; } catch { /* Use defaults. */ }
  }
  return Object.fromEntries(ADES.map(({ id }) => {
    const values = defaultsFor(id);
    for (const { key, min, max } of [...controls, ...patternControls, ...lightControls, ...orcaControls]) {
      const value = stored[id]?.[key];
      if (typeof value === 'number' && Number.isFinite(value)) values[key] = Math.max(min, Math.min(max, value));
    }
    values.orcaPaused = stored[id]?.orcaPaused === 1 ? 1 : 0;
    // Apply the requested opacity preset once without discarding other tuning.
    if (id === 'paradev' && stored.paradevOpacityVersion !== 1) {
      values.backgroundOpacity = defaultsFor(id).backgroundOpacity;
      values.cubeOpacity = 100;
    }
    values.fadeEnd = Math.max(values.fadeStart + 5, values.fadeEnd);
    return [id, values];
  })) as SettingsByAde;
}

export function useHeroSettings(ade: Ade) {
  const [all, setAll] = useState(readSettings);
  const [saved, setSaved] = useState(true);
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...all, paradevOpacityVersion: 1 })); setSaved(true); }
    catch { setSaved(false); }
  }, [all]);
  // Fast Refresh preserves older state objects: fill in newly introduced controls.
  const values = { ...defaultsFor(ade), ...all[ade], ...normalizeParadevSettings({ ...defaultsFor(ade), ...all[ade] }) };
  const change = (key: keyof Settings, value: number) => setAll((previous) => {
    const next = { ...defaultsFor(ade), ...previous[ade], ...normalizeParadevSettings({ ...defaultsFor(ade), ...previous[ade] }), [key]: value };
    if (key === 'fadeStart') next.fadeEnd = Math.max(next.fadeEnd, value + 5);
    if (key === 'fadeEnd') next.fadeStart = Math.min(next.fadeStart, value - 5);
    return { ...previous, [ade]: next };
  });
  const reset = () => setAll((previous) => ({ ...previous, [ade]: defaultsFor(ade) }));
  const style = {
    '--hero-background-opacity': values.backgroundOpacity / 100,
    '--hero-cube-opacity': values.cubeOpacity / 100,
    '--hero-fade-start': `${values.fadeStart}%`,
    '--hero-fade-end': `${values.fadeEnd}%`,
    '--hero-cube-y': `${values.cubeY}%`,
    '--paradev-light-opacity': values.paradevLightOpacity / 100,
    '--paradev-light-source-width': `${36 * values.paradevLightWidth / 100}%`,
    '--paradev-light-width': `${55 * values.paradevLightWidth / 100}%`,
    '--paradev-light-reach': `${values.paradevLightReach}%`,
    '--orca-duration': `${2800 / values.orcaSpeed}s`,
    '--orca-delay': `${-700 / values.orcaSpeed}s`,
    '--orca-swim-duration': `${560 / values.orcaSpeed}s`,
    '--orca-distant-duration': `${3900 / values.orcaSpeed}s`,
    '--orca-distant-delay': `${-2300 / values.orcaSpeed}s`,
    '--orca-distant-swim-duration': `${740 / values.orcaSpeed}s`,
    '--orca-distant-swim-delay': `${-300 / values.orcaSpeed}s`,
    '--orca-light-duration': `${1800 / values.orcaSpeed}s`,
    '--orca-water-light-duration': `${1400 / values.orcaSpeed}s`,
    '--orca-particle-duration': `${2400 / values.orcaSpeed}s`,
    '--orca-play-state': values.orcaPaused ? 'paused' : 'running',
    '--orca-width': `${210 * values.orcaScale / 100}px`,
    '--orca-distant-width': `${116 * values.orcaScale / 100}px`,
    '--orca-opacity': values.orcaOpacity / 100,
    '--orca-y': `${values.orcaY}%`,
  } as CSSProperties;
  return { values, style, change, reset, saved };
}
