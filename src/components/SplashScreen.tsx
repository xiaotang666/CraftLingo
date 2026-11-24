import { useEffect, useRef, useState, useCallback } from "react";
import { cancelWarmup } from "../api/tauri";
import { normalizeAppLanguage, t } from "../i18n/translations";
import type { WarmupPhase, WarmupProgress } from "../types";

// ── Constants ──────────────────────────────────────────────────────────

const SPLASH_MIN_MS = 3000;
const LETTER_REVEAL_MS = 60;
/** Primary title (Chinese), revealed letter by letter. */
const BRAND_NAME = "方块译匠";
/** Secondary title (English), shown as the subtitle. */
const BRAND_NAME_EN = "CraftLingo";
const ICON_SIZE = 48;

type SplashPhaseKey = Exclude<WarmupPhase, "completed">;
const PHASE_ORDER: SplashPhaseKey[] = ["settings", "local", "dictionary", "llm"];

interface SplashScreenProps {
  /** Called when the splash sequence is complete and ready to transition to main UI. */
  onFinish: () => void;
  /** Error message if warmup failed fatally (user can force-skip). */
  fatalError?: string;
  /** Whether we're in offline mode (LLM unreachable). */
  offline?: boolean;
  /** Whether this is the very first app launch. */
  isFirstLaunch?: boolean;
  /** Is warmup complete (from event). */
  warmupComplete?: boolean;
  /** Current warmup progress from Rust events. */
  progress?: WarmupProgress | null;
  /** App language for splash screen i18n. */
  language?: string;
}

// ── Isometric block + translate-arrows icon ───────────────────────────

function CraftLingoIcon({ size }: { size: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 48 48"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className="splash-icon-svg"
    >
      {/* Isometric cube: top / left / right faces */}
      <polygon points="24,3 43,13 24,23 5,13" fill="var(--accent)" opacity="0.95" />
      <polygon points="5,13 24,23 24,45 5,35" fill="var(--accent)" opacity="0.5" />
      <polygon points="43,13 24,23 24,45 43,35" fill="var(--accent)" opacity="0.75" />
      {/* Bidirectional arrows on the front face: language exchange */}
      <path
        d="M30 28.5 H38 M35.6 26 L38 28.5 L35.6 31"
        stroke="var(--bg-surface, #fff)"
        strokeWidth="1.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M38 36.5 H30 M32.4 34 L30 36.5 L32.4 39"
        stroke="var(--bg-surface, #fff)"
        strokeWidth="1.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

// ── Main component ─────────────────────────────────────────────────────

export function SplashScreen({
  onFinish,
  fatalError,
  offline,
  isFirstLaunch,
  warmupComplete,
  progress,
  language,
}: SplashScreenProps) {
  const splashLang = normalizeAppLanguage(language ?? "zh_cn");
  const [phase, setPhase] = useState<SplashPhase>("init");
  const [showSplash, setShowSplash] = useState(true);
  const [curPercent, setCurPercent] = useState(0);
  const [curPhase, setCurPhase] = useState<WarmupPhase | null>(null);
  const [phaseStatuses, setPhaseStatuses] = useState<
    Record<string, "running" | "completed" | "failed">
  >({});

  const startTimeRef = useRef(Date.now());
  const transitionTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const phaseCompleteTimersRef = useRef<Record<string, ReturnType<typeof setTimeout>>>({});

  // ── Track warmup progress events ──
  useEffect(() => {
    if (!progress) return;

    setCurPercent((prev) => Math.max(prev, progress.percent));
    setCurPhase(progress.phase);

    if (progress.phase !== "completed") {
      if (progress.status === "completed") {
        if (phaseCompleteTimersRef.current[progress.phase]) {
          clearTimeout(phaseCompleteTimersRef.current[progress.phase]);
        }
        setPhaseStatuses((prev) => ({
          ...prev,
          [progress.phase]: prev[progress.phase] === "completed" ? "completed" : "running",
        }));
        phaseCompleteTimersRef.current[progress.phase] = setTimeout(() => {
          setPhaseStatuses((prev) => ({
            ...prev,
            [progress.phase]: "completed",
          }));
          delete phaseCompleteTimersRef.current[progress.phase];
        }, 260);
      } else {
        setPhaseStatuses((prev) => ({
          ...prev,
          [progress.phase]: progress.status,
        }));
      }
    }
  }, [progress]);

  useEffect(() => () => {
    Object.values(phaseCompleteTimersRef.current).forEach(clearTimeout);
    phaseCompleteTimersRef.current = {};
  }, []);

  // When warmup completes, handle the minimum display time and transition
  useEffect(() => {
    if (!warmupComplete && !fatalError) return;

    const elapsed = Date.now() - startTimeRef.current;
    const remaining = Math.max(0, SPLASH_MIN_MS - elapsed);

    transitionTimerRef.current = setTimeout(() => {
      // Start fade-out
      setShowSplash(false);

      // After fade out, trigger main fade in
      setTimeout(() => {
        onFinish();
      }, 400); // fade-out duration
    }, remaining);

    return () => {
      if (transitionTimerRef.current) {
        clearTimeout(transitionTimerRef.current);
      }
    };
  }, [warmupComplete, fatalError, onFinish]);

  // ── Brand name letter reveal logic ──
  useEffect(() => {
    const timer = setTimeout(() => setPhase("brand"), 500); // after icon float-up
    return () => clearTimeout(timer);
  }, []);

  // ── Phase label helpers ──
  const getPhaseLabel = (phaseKey: SplashPhaseKey): string =>
    t(splashLang, `splash.phase.${phaseKey}`);

  function getPhaseIcon(phaseKey: SplashPhaseKey): string {
    const status = phaseStatuses[phaseKey];
    if (status === "completed") return "\u2713";
    if (status === "failed") return "\u26A0";
    if (phaseKey === curPhase && status === "running") return "\u27F3";
    return "";
  }

  const hasAnimationStarted = phase !== "init";

  // ── Error fallback button ──
  const handleForceSkip = useCallback(() => {
    // Cancel background warmup on Rust side
    cancelWarmup().catch(() => {});
    // Clear any pending warmup-completion transition timer
    if (transitionTimerRef.current) {
      clearTimeout(transitionTimerRef.current);
      transitionTimerRef.current = null;
    }
    setShowSplash(false);
    setTimeout(() => {
      onFinish();
    }, 400);
  }, [onFinish]);

  // ── Render ──
  return (
      <div
        className={`splash-overlay${showSplash ? " visible" : " fading-out"}`}
      >
        <div className="splash-content">
          {/* Icon */}
          <div className="splash-icon-wrapper">
            <CraftLingoIcon size={ICON_SIZE} />
          </div>

          {/* Brand name with letter reveal */}
          <h1 className="splash-brand">
            {BRAND_NAME.split("").map((letter, i) => (
              <span
                key={i}
                className="splash-letter"
                style={{
                  opacity: 0,
                  animation:
                    hasAnimationStarted
                      ? `splashLetterReveal 60ms ease-out ${500 + i * LETTER_REVEAL_MS}ms forwards`
                      : undefined,
                }}
              >
                {letter}
              </span>
            ))}
          </h1>

          {/* Subtitle */}
          <p
            className="splash-subtitle"
            style={{
              opacity: 0,
              animation:
                hasAnimationStarted
                  ? "splashFadeIn 200ms ease-out forwards"
                  : undefined,
              animationDelay: `${500 + BRAND_NAME.length * LETTER_REVEAL_MS + 100}ms`,
            }}
          >
            {BRAND_NAME_EN}
          </p>

          {/* Scan line */}
          <div className="splash-scanline" />

          {/* Progress bar + phase steps */}
          <div className="splash-progress-section">
            <div className="splash-progress-track">
              <div
                className="splash-progress-fill"
                style={{ width: `${curPercent}%` }}
              />
            </div>

            <div className="splash-phase-steps">
              {PHASE_ORDER.map((phaseKey) => (
                <div
                  key={phaseKey}
                  className={`splash-phase-step${
                    phaseKey === curPhase && phaseStatuses[phaseKey] === "running"
                      ? " active"
                      : phaseStatuses[phaseKey] === "completed"
                        ? " done"
                        : phaseStatuses[phaseKey] === "failed"
                          ? " failed"
                          : ""
                  }`}
                >
                  <span className="splash-phase-icon">
                    {getPhaseIcon(phaseKey)}
                  </span>
                  <span className="splash-phase-label">
                    {getPhaseLabel(phaseKey)}
                  </span>
                </div>
              ))}
            </div>
          </div>

          {/* Status messages */}
          {progress?.message && !fatalError && (
            <p className="splash-status">{progress.message}</p>
          )}

          {/* Offline indicator */}
          {offline && !fatalError && (
            <p className="splash-offline-indicator">{t(splashLang, "splash.offline")}</p>
          )}

          {/* First launch hint */}
          {isFirstLaunch && !warmupComplete && (
            <p className="splash-first-launch-hint">
              {t(splashLang, "splash.firstLaunch")}
            </p>
          )}

          {/* Fatal error with skip button */}
          {fatalError && (
            <div className="splash-error">
              <p className="splash-error-msg">{fatalError}</p>
              <button
                className="splash-skip-btn"
                onClick={handleForceSkip}
                type="button"
              >
                {t(splashLang, "splash.skip")}
              </button>
            </div>
          )}
        </div>
      </div>
  );
}

type SplashPhase = "init" | "brand";
