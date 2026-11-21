import { adeName, type Ade } from "./ade";
import { adeLogoUrl } from "./hero/ade-logo";
import t3Icon from "./assets/machine-card/t3.svg";

export default function AdeLogo({ ade, decorative = false }: { ade: Ade; decorative?: boolean }) {
  const src = ade === "t3" ? t3Icon : adeLogoUrl(ade);
  return <span className="card-ade-logo" data-ade={ade} title={adeName(ade)}>
    <img src={src} alt={decorative ? "" : adeName(ade)} />
  </span>;
}
