import { lazy, Suspense, useEffect, useRef, useState } from "react";
import symbol from "./assets/main-view/instabox-symbol.svg";
import AdeSwitcher from "./AdeSwitcher";
import type { Mode } from "./ades";
import { themeOf } from "./hero/AppBackground";
import HeroAtmosphere from "./hero/HeroAtmosphere";
import { useHeroSettings } from "./hero/HeroSettings";
import type { AppState } from "./types";

// The cube is a browser preview layout only, so the desktop build leaves out its renderer.
const HeroCube = import.meta.env.DEV ? lazy(() => import("./hero/HeroCube").then((m) => ({ default: m.HeroCube }))) : null;

interface Props {
  state: AppState;
  mode: Mode;
  switcherOpen: boolean;
  onSwitcherOpenChange: (open: boolean) => void;
  onChoose: (mode: Mode) => void;
  showHero?: boolean;
}

/** The list's header: the instabox mark and the ADE switcher, over the mode's background. */
export default function MainHeader({ state, mode, switcherOpen, onSwitcherOpenChange, onChoose, showHero = false }: Props) {
  const theme = themeOf(mode);
  const settings = useHeroSettings(theme);
  const toolbar = useRef<HTMLElement>(null);
  const [stuck, setStuck] = useState(false);

  useEffect(() => {
    if (!showHero) return;
    const nav = toolbar.current;
    const scroll = nav?.closest(".scroll");
    const heading = scroll?.querySelector(".list-head");
    if (!nav || !scroll || !heading) return;
    // The solid header begins when the sandbox heading docks below the nav.
    const update = () => setStuck(heading.getBoundingClientRect().top <= nav.getBoundingClientRect().bottom);
    update();
    scroll.addEventListener("scroll", update, { passive: true });
    window.addEventListener("resize", update);
    return () => {
      scroll.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
    };
  }, [showHero]);

  return (
    <>
      <header ref={toolbar} className={`main-hero-toolbar${showHero ? "" : " main-nav-compact"}`} data-stuck={stuck}>
        <div className="main-wordmark" role="img" aria-label="InstaBox">
          <img className="main-wordmark-symbol" src={symbol} alt="" />
        </div>
        <AdeSwitcher state={state} mode={mode} open={switcherOpen} onOpenChange={onSwitcherOpenChange} onChoose={onChoose} />
      </header>
      {showHero && HeroCube && (
        <div className="main-hero" style={settings.style}>
          <HeroAtmosphere ade={theme} patternSettings={settings.values} />
          <Suspense fallback={null}>
            <HeroCube ade={theme} />
          </Suspense>
        </div>
      )}
    </>
  );
}
