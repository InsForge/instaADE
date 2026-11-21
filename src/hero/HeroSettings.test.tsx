import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, test } from 'vitest';
import type { Ade } from '../ade';
import { heroSettingsDefaults, useHeroSettings } from './HeroSettings';

let settings: ReturnType<typeof useHeroSettings>;
function Preview({ ade }: { ade: Ade }) {
  settings = useHeroSettings(ade);
  return <header data-testid="hero" style={settings.style} />;
}
const adjust = (key: keyof typeof heroSettingsDefaults, value: number) => act(() => settings.change(key, value));
const reset = () => act(() => settings.reset());
const value = (name: string) => screen.getByTestId('hero').style.getPropertyValue(name);
beforeEach(() => localStorage.clear());
afterEach(() => { cleanup(); localStorage.clear(); });

test('opacity is live, independent per ADE, persists after remount, and resets only the current ADE', () => {
  const view = render(<Preview ade="cmux" />);
  adjust('backgroundOpacity', 35);
  expect(value('--hero-background-opacity')).toBe('0.35');
  expect(value('--hero-cube-opacity')).toBe('1');
  view.rerender(<Preview ade="orca" />);
  expect(value('--hero-background-opacity')).toBe('1');
  adjust('backgroundOpacity', 60);
  view.rerender(<Preview ade="cmux" />);
  expect(value('--hero-background-opacity')).toBe('0.35');
  expect(settings.saved).toBe(true);
  view.unmount();
  const remount = render(<Preview ade="cmux" />);
  expect(value('--hero-background-opacity')).toBe('0.35');
  reset();
  expect(value('--hero-background-opacity')).toBe('0.4');
  expect(value('--hero-fade-start')).toBe('0%');
  expect(value('--hero-fade-end')).toBe('100%');
  expect(value('--hero-cube-y')).toBe('64%');
  remount.rerender(<Preview ade="orca" />);
  expect(value('--hero-background-opacity')).toBe('0.6');
});

test('fade bounds stay ordered', () => {
  render(<Preview ade="paradev" />);
  adjust('fadeStart', 95);
  expect(value('--hero-fade-end')).toBe('100%');
  adjust('fadeEnd', 20);
  expect(value('--hero-fade-start')).toBe('15%');
});

test('malformed stored values fall back or clamp to valid ranges', () => {
  localStorage.setItem('instabox.hero-preview.v1', JSON.stringify({ cmux: { backgroundOpacity: -20, cubeY: 1000, fadeStart: 90, fadeEnd: 10 } }));
  const view = render(<Preview ade="cmux" />);
  expect(value('--hero-background-opacity')).toBe('0');
  expect(value('--hero-cube-y')).toBe('80%');
  expect(value('--hero-fade-end')).toBe('95%');
  view.unmount();
  localStorage.setItem('instabox.hero-preview.v1', 'invalid json');
  render(<Preview ade="cmux" />);
  expect(value('--hero-background-opacity')).toBe('0.4');
});

test('Paradev applies the opacity preset once and preserves pattern tuning across ADEs and reloads', () => {
  localStorage.setItem('instabox.hero-preview.v1', JSON.stringify({ paradev: { backgroundOpacity: 90, cubeOpacity: 70, fadeStart: 25 }, cmux: { backgroundOpacity: 35 } }));
  const view = render(<Preview ade="paradev" />);
  expect(value('--hero-background-opacity')).toBe('0.48');
  expect(value('--hero-cube-opacity')).toBe('1');
  expect(value('--hero-fade-start')).toBe('25%');
  adjust('pixelSize', 4);
  adjust('pixelX', -25);
  adjust('pixelY', 30);
  adjust('pixelOpacity', 65);
  adjust('pixelDensity', 150);
  adjust('pixelSpeed', 0);
  adjust('backgroundOpacity', 50);
  view.rerender(<Preview ade="cmux" />);
  expect(value('--hero-background-opacity')).toBe('0.35');
  view.unmount();
  render(<Preview ade="paradev" />);
  expect(value('--hero-background-opacity')).toBe('0.5');
  expect(settings.values).toMatchObject({ pixelX: -25, pixelY: 30, pixelSize: 4, pixelOpacity: 65, pixelDensity: 150, pixelSpeed: 0 });
  reset();
  expect(value('--hero-background-opacity')).toBe('0.48');
  expect(value('--hero-fade-start')).toBe('61%');
  expect(value('--hero-fade-end')).toBe('100%');
  expect(settings.values).toMatchObject({ pixelSize: 2, pixelX: 68, pixelY: 22 });
});

test('Orca settings update the preview, persist independently, and reset with the current ADE', () => {
  const view = render(<Preview ade="orca" />);
  adjust('orcaSpeed', 200);
  adjust('orcaScale', 150);
  adjust('orcaOpacity', 30);
  adjust('orcaY', 65);
  adjust('orcaPaused', 1);
  expect(value('--orca-duration')).toBe('14s');
  expect(value('--orca-distant-width')).toBe('174px');
  expect(value('--orca-width')).toBe('315px');
  expect(value('--orca-opacity')).toBe('0.3');
  expect(value('--orca-y')).toBe('65%');
  expect(value('--orca-play-state')).toBe('paused');
  view.rerender(<Preview ade="cmux" />);
  adjust('backgroundOpacity', 25);
  view.unmount();

  const remount = render(<Preview ade="orca" />);
  expect(value('--orca-duration')).toBe('14s');
  expect(value('--orca-opacity')).toBe('0.3');
  expect(value('--orca-play-state')).toBe('paused');
  adjust('orcaPaused', 0);
  expect(value('--orca-play-state')).toBe('running');
  reset();
  expect(value('--orca-duration')).toBe('28s');
  expect(value('--orca-width')).toBe('273px');
  expect(value('--orca-opacity')).toBe('0.2');
  expect(value('--orca-y')).toBe('25%');
  remount.rerender(<Preview ade="cmux" />);
  expect(value('--hero-background-opacity')).toBe('0.25');
});

test('Orca settings safely load old preferences and reject invalid speed or pause values', () => {
  localStorage.setItem('instabox.hero-preview.v1', JSON.stringify({ orca: { backgroundOpacity: 70 } }));
  const view = render(<Preview ade="orca" />);
  expect(value('--hero-background-opacity')).toBe('0.7');
  expect(value('--orca-duration')).toBe('28s');
  expect(value('--orca-play-state')).toBe('running');
  view.unmount();
  localStorage.setItem('instabox.hero-preview.v1', JSON.stringify({ orca: { orcaSpeed: 0, orcaScale: 999, orcaOpacity: -20, orcaY: 'bad', orcaPaused: 'true' } }));
  render(<Preview ade="orca" />);
  expect(value('--orca-duration')).toBe('112s');
  expect(value('--orca-width')).toBe('630px');
  expect(value('--orca-opacity')).toBe('0');
  expect(value('--orca-y')).toBe('25%');
  expect(value('--orca-play-state')).toBe('running');
});
