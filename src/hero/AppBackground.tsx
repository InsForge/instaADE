import type { Ade } from "../ade";
import HeroAtmosphere from "./HeroAtmosphere";
import { useHeroSettings } from "./HeroSettings";

export default function AppBackground({ ade }: { ade: Ade }) {
  const settings = useHeroSettings(ade);

  return <div className="app-background" style={settings.style} aria-hidden="true">
    <div className="main-hero">
      <HeroAtmosphere ade={ade} patternSettings={settings.values} backgroundOnly={ade !== "paradev"} />
    </div>
  </div>;
}
