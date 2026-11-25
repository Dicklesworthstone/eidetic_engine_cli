"use client";

import {
  Anchor,
  BookmarkPlus,
  Check,
  ChevronLeft,
  ChevronRight,
  Compass,
  Copy,
  Footprints,
  Hammer,
  type LucideIcon,
  Package,
  Pause,
  Play,
  Radar,
  RotateCcw,
  Scale,
  Send,
} from "lucide-react";
import {
  type FocusEvent,
  Fragment,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";

import { DemoButton, DemoFrame } from "@/components/explain/demo";
import { Heading } from "@/components/heading-level";
import {
  arcPath,
  commandTokens,
  type CommandToken,
  type EffectKind,
  jsonTokens,
  type JsonToken,
  MAX_JSON_LINES,
  ringGeometry,
  type RingGeometry,
  shortestDelta,
  type Station,
  STATION_COUNT,
  stationAngle,
  STATIONS,
  STEP_DEG,
  STEP_MS,
  toRad,
} from "@/components/explain/session-loop-data";
import { useFigureWidth, useInView, useReducedMotion } from "@/lib/hooks";
import { cn } from "@/lib/utils";

/** The chapter's lime: the stations that only read, the marker, and the paths an agent reads. */
const CHAPTER = "var(--chapter,#a3e635)";
/** The write direction's amber, as in the memory loop (loop.tsx). */
const WRITE = "#fbbf24";
/** The harness's own step: ee is not called. */
const HARNESS = "#94a3b8";

const EFFECT_COLOUR: Record<EffectKind, string> = { read: CHAPTER, write: WRITE, harness: HARNESS };

const ICONS: Record<string, LucideIcon> = {
  resume: Footprints,
  orient: Compass,
  pack: Package,
  recall: Anchor,
  preflight: Radar,
  work: Hammer,
  remember: BookmarkPlus,
  outcome: Scale,
  handoff: Send,
};

const N = STATION_COUNT;
const pad = (value: number) => String(value).padStart(2, "0");
const stationAt = (index: number): Station => STATIONS[index] ?? STATIONS[0]!;

/** `ee pack`, `ee preflight check`: the command without its flags and arguments. */
function commandName(station: Station): string {
  if (!station.command) return "no ee call";
  return commandTokens(station.command)
    .filter((token) => token.kind === "bin" || token.kind === "sub")
    .map((token) => token.text)
    .join(" ");
}

/** A station's name in a list: its command, or for the harness's own step, what it is. */
const rowName = (station: Station): string => (station.command ? commandName(station) : station.name);

type Mode = "stack" | "top" | "side";

/** A phone gets a stepper; a tablet the ring above its panel; a wide screen the two side by side. */
function modeFor(width: number): Mode {
  if (width < 600) return "stack";
  if (width < 960) return "top";
  return "side";
}

const LABEL = "text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase";
const TRAVEL_MS = 900;

/**
 * One session as an agent runs it: nine stations around a ring, from `ee resume` to `ee handoff create`
 * and back. Five only read, three write, and one is the harness's own work. A marker travels the ring;
 * each station's panel shows the exact command, the JSON paths the agent reads, what comes back, and
 * why the surface is shaped for an agent rather than a person.
 */
export function SessionLoop() {
  const { ref, width } = useFigureWidth(1000);
  const reduced = useReducedMotion();
  const inView = useInView(ref);
  const mode = modeFor(width);

  const [active, setActive] = useState(0);
  /** Signed stations travelled so far: the marker's rotation, so it never spins back to go forward. */
  const [turn, setTurn] = useState(0);
  const [direction, setDirection] = useState(1);
  const [moving, setMoving] = useState(false);
  const [session, setSession] = useState(1);
  /** Counts the progress arc's resets, so a new session starts it empty instead of drawing it backwards. */
  const [laps, setLaps] = useState(0);
  /**
   * The reader's choice. Until they make one, a wide figure plays, and a phone's stepper waits, as does
   * every layout when the reader prefers reduced motion: there it moves only if they press Play.
   */
  const [choice, setChoice] = useState<boolean | null>(null);
  /** The pointer is over the panel. */
  const [hold, setHold] = useState(false);
  /** Keyboard focus is on a station or in a panel: a focused copy button must not vanish under it. */
  const [focusHold, setFocusHold] = useState(false);

  const playing = choice ?? (mode !== "stack" && !reduced);
  const animate = !reduced;
  const running = playing && inView && !hold && !focusHold;
  // React's ids carry punctuation that a url(#…) reference cannot.
  const uid = `sl${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const tabs = useRef<(SVGGElement | null)[]>([]);

  const go = (to: number, byReader: boolean) => {
    const target = ((to % N) + N) % N;
    const delta = shortestDelta(active, target);
    if (byReader) setChoice(false);
    if (delta === 0) return;
    const crossed = active + delta >= N ? 1 : active + delta < 0 ? -1 : 0;
    if (crossed !== 0) {
      setSession((value) => Math.max(1, value + crossed));
      setLaps((value) => value + 1);
    }
    setTurn((value) => value + delta);
    setDirection(delta > 0 ? 1 : -1);
    setActive(target);
    if (animate) setMoving(true);
  };

  // Until the reader takes over, the session moves on by itself.
  useEffect(() => {
    if (!running) return;
    const timer = window.setTimeout(() => go(active + 1, false), STEP_MS);
    return () => window.clearTimeout(timer);
    // `go` reads `active`, which is a dependency; it is otherwise stable.
  }, [running, active]); // eslint-disable-line react-hooks/exhaustive-deps

  // A layout change can unmount the panel under the pointer before it reports leaving.
  useEffect(() => setHold(false), [mode]);

  // The comet's tail shows only while the marker travels.
  useEffect(() => {
    if (!moving) return;
    const timer = window.setTimeout(() => setMoving(false), TRAVEL_MS + 60);
    return () => window.clearTimeout(timer);
  }, [moving, turn]);

  // Reading a panel holds the session still; the ring itself does not, so the marker can be watched.
  const holdWhileOver = { onPointerEnter: () => setHold(true), onPointerLeave: () => setHold(false) };

  // The transport buttons are exempt: focus on Play must not stop what it just started.
  const outsideControls = (node: EventTarget | null) =>
    node instanceof Element && ref.current?.contains(node) === true && node.closest("[data-controls]") === null;
  const onFocusIn = (event: FocusEvent) => setFocusHold(outsideControls(event.target));
  const onFocusOut = (event: FocusEvent) => setFocusHold(outsideControls(event.relatedTarget));

  const onTabKey = (event: KeyboardEvent<SVGGElement>, index: number) => {
    const moves: Record<string, number> = {
      ArrowRight: index + 1,
      ArrowDown: index + 1,
      ArrowLeft: index - 1,
      ArrowUp: index - 1,
      Home: 0,
      End: N - 1,
    };
    const next = moves[event.key];
    if (next === undefined) {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        go(index, true);
      }
      return;
    }
    event.preventDefault();
    const target = (next + N) % N;
    go(target, true);
    tabs.current[target]?.focus();
  };

  const controls = (
    <div data-controls className="flex items-center gap-1.5">
      <DemoButton onClick={() => go(active - 1, true)} label="Previous station" className="px-2.5">
        <ChevronLeft className="h-3.5 w-3.5" aria-hidden="true" />
      </DemoButton>
      {/* No aria-pressed: the label already names the state's opposite, and both would read "Pause, pressed". */}
      <DemoButton onClick={() => setChoice(!playing)} label={playing ? "Pause the session" : "Play the session"}>
        {playing ? <Pause className="h-3.5 w-3.5" aria-hidden="true" /> : <Play className="h-3.5 w-3.5" aria-hidden="true" />}
        {playing ? "Pause" : "Play"}
      </DemoButton>
      <DemoButton onClick={() => go(active + 1, true)} label="Next station" className="px-2.5">
        <ChevronRight className="h-3.5 w-3.5" aria-hidden="true" />
      </DemoButton>
    </div>
  );

  const ringWidth = mode === "side" ? Math.round(Math.min(540, Math.max(440, width * 0.46))) : Math.min(width, 600);
  const geometry = ringGeometry(ringWidth);

  return (
    <DemoFrame
      eyebrow="For agents · one session, command by command"
      title="The ee commands in one agent session."
      basis="diagram"
      source={
        <>
          README.md · Agent Operating Loop · src/cli/mod.rs · ResumeArgs, OrientArgs, PackArgs (--read-only), RecallArgs
          (--diff-staged), PreflightGuardArgs, RememberArgs (--idempotency-key), OutcomeArgs (--event-id),
          HandoffCreateArgs, handle_resume, handle_orient · src/core/effect.rs (which commands read and which write) ·
          src/core/resume.rs · src/core/preflight_guard.rs · src/core/outcome.rs (DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR 5,
          DEFAULT_HARMFUL_BURST_WINDOW_SECONDS 3600)
          · src/core/handoff.rs · src/output/mod.rs (build_pack_skipped_item, render_handoff_create_json) · docs/schemas
          · ee.response.v2, ee.resume.v1, ee.orient.v1, ee.pack.v2, ee.recall.v1, ee.preflight.guard.v1. Field names
          are spelled as those files spell them; each excerpt is a selection of fields, values are illustrative, and …
          marks a cut.
          web/tests/session-loop.test.ts checks every key against the files each station cites.
        </>
      }
    >
      <div ref={ref} className="min-w-0" onFocus={onFocusIn} onBlur={onFocusOut}>
        <p className="max-w-3xl text-[15px] leading-relaxed text-slate-300">
          Nine stations: five read from ee, three write to it, one is the harness&apos;s own work. Pick one to see its
          command and the JSON it returns, or press Play.
        </p>

        {mode === "stack" ? (
          <Stepper
            active={active}
            session={session}
            running={running}
            animate={animate}
            pulse={inView && animate}
            uid={uid}
            controls={controls}
            onSelect={(index) => go(index, true)}
          />
        ) : (
          <div
            className={cn("mt-6 grid gap-7", mode === "side" ? "items-start" : "grid-cols-1")}
            style={mode === "side" ? { gridTemplateColumns: `minmax(0,${geometry.W}px) minmax(0,1fr)` } : undefined}
          >
            <div className={cn("min-w-0", mode === "top" && "mx-auto")} style={{ width: geometry.W, maxWidth: "100%" }}>
              <Ring
                g={geometry}
                active={active}
                turn={turn}
                direction={direction}
                moving={moving && animate}
                animate={animate}
                pulse={inView && animate}
                session={session}
                laps={laps}
                uid={uid}
                tabs={tabs}
                onSelect={(index) => go(index, true)}
                onKey={onTabKey}
              />
              <div className="mt-3 flex flex-wrap items-center justify-between gap-x-4 gap-y-3">
                <Legend />
                {controls}
              </div>
              {mode === "side" ? (
                <div className="mt-5 border-t border-white/[0.07] pt-4" {...holdWhileOver}>
                  <Crossfade
                    active={active}
                    animate={animate}
                    uid={uid}
                    panels={false}
                    render={(station, index) => <StationDetail station={station} index={index} variant="why" />}
                  />
                </div>
              ) : null}
            </div>

            <div className="relative min-w-0 rounded-2xl border border-white/[0.08] bg-black/25 p-4 sm:p-5" {...holdWhileOver}>
              <Countdown running={running} restart={active} />
              <Crossfade
                active={active}
                animate={animate}
                uid={uid}
                panels
                render={(station, index) => (
                  <StationDetail
                    station={station}
                    index={index}
                    variant={mode === "side" ? "main" : width >= 800 ? "split" : "full"}
                    fixedHeight
                  />
                )}
              />
            </div>
          </div>
        )}
      </div>
    </DemoFrame>
  );
}

/** The ring: an orbit with nine stations, a track coloured by what each does, the session's progress, and the marker. */
function Ring({
  g,
  active,
  turn,
  direction,
  moving,
  animate,
  pulse,
  session,
  laps,
  uid,
  tabs,
  onSelect,
  onKey,
}: {
  g: RingGeometry;
  active: number;
  turn: number;
  direction: number;
  moving: boolean;
  animate: boolean;
  pulse: boolean;
  session: number;
  laps: number;
  uid: string;
  tabs: RefObject<(SVGGElement | null)[]>;
  onSelect: (index: number) => void;
  onKey: (event: KeyboardEvent<SVGGElement>, index: number) => void;
}) {
  const { W, H, cx, cy, R } = g;
  const station = stationAt(active);
  const colour = EFFECT_COLOUR[station.effect];
  const circumference = 2 * Math.PI * R;
  const done = (active / N) * circumference;
  const handoff = stationAngle(N - 1);
  const resume = stationAngle(0) + 360;
  const between = (handoff + resume) / 2;
  const ease = "cubic-bezier(0.65, 0, 0.35, 1)";
  const track = R - 13;
  const textRadius = R - 41;
  const hubFont = R >= 150 ? 17 : 15;
  const hubLines = station.hub.length;
  const hubSpan = 24 + (hubLines - 1) * (hubFont + 4) + 24;
  const hubTop = cy - hubSpan / 2 + 3;

  return (
    <svg
      className="diagram block overflow-visible"
      width={W}
      height={H}
      // Before the first measurement the width is a guess; scale down rather than overflow a phone.
      style={{ maxWidth: "100%", height: "auto" }}
      viewBox={`0 0 ${W} ${H}`}
      role="group"
      aria-label="The session loop: nine stations around a ring, from ee resume to ee handoff create and back to resume for the next session. Choose a station to read its command, the JSON paths an agent reads, and what comes back."
    >
      <defs>
        <radialGradient id={`${uid}-glow`}>
          <stop offset="0" style={{ stopColor: CHAPTER, stopOpacity: 0.6 }} />
          <stop offset="0.45" style={{ stopColor: CHAPTER, stopOpacity: 0.22 }} />
          <stop offset="1" style={{ stopColor: CHAPTER, stopOpacity: 0 }} />
        </radialGradient>
        <radialGradient id={`${uid}-hub`}>
          <stop offset="0" style={{ stopColor: colour, stopOpacity: 0.11 }} />
          <stop offset="1" style={{ stopColor: colour, stopOpacity: 0 }} />
        </radialGradient>
        <path id={`${uid}-between`} d={arcPath(cx, cy, textRadius, between - 60, between + 60)} />
      </defs>

      <g aria-hidden="true">
        {/* The hub's wash, in the colour of what the station does. */}
        <circle cx={cx} cy={cy} r={R - 20} fill={`url(#${uid}-hub)`} className="transition-[fill] duration-500" />

        {/* A dial: ticks every five degrees inside the track. */}
        {Array.from({ length: 72 }, (_, index) => {
          if (index % 8 === 0) return null;
          const angle = toRad(index * 5 - 90);
          const inner = R - 25;
          const outer = R - (index % 4 === 0 ? 30 : 28);
          return (
            <line
              key={index}
              x1={cx + inner * Math.cos(angle)}
              y1={cy + inner * Math.sin(angle)}
              x2={cx + outer * Math.cos(angle)}
              y2={cy + outer * Math.sin(angle)}
              stroke="rgba(221,214,254,0.13)"
              strokeWidth="1"
            />
          );
        })}

        {/* The track: each station's stretch, coloured by whether it reads, writes, or belongs to the harness. */}
        {STATIONS.map((item, index) => {
          const angle = stationAngle(index);
          return (
            <path
              key={item.id}
              d={arcPath(cx, cy, track, angle - STEP_DEG / 2 + 2.5, angle + STEP_DEG / 2 - 2.5)}
              fill="none"
              style={{ stroke: EFFECT_COLOUR[item.effect] }}
              strokeOpacity={index === active ? 0.85 : 0.32}
              strokeWidth={index === active ? 3.5 : 2.5}
              strokeLinecap="round"
              strokeDasharray={item.effect === "harness" ? "2 4" : undefined}
              className="transition-[stroke-opacity,stroke-width] duration-500"
            />
          );
        })}

        {/* The orbit, and the stretch between one session's handoff and the next one's resume. */}
        <path d={arcPath(cx, cy, R, stationAngle(0), handoff)} fill="none" stroke="rgba(221,214,254,0.16)" strokeWidth="1.25" />
        <path d={arcPath(cx, cy, R, handoff, resume)} fill="none" stroke="#77839c" strokeWidth="1.25" strokeDasharray="2 5" />
        <text className="mono" fontSize="8.5" letterSpacing="1.6" fill="#77839c">
          <textPath href={`#${uid}-between`} startOffset="50%" textAnchor="middle">
            NEXT SESSION
          </textPath>
        </text>

        {/* How far this session has come. Re-keyed on a new lap, so it starts empty rather than unwinding. */}
        <g key={laps} transform={`rotate(-90 ${cx} ${cy})`}>
          <circle
            cx={cx}
            cy={cy}
            r={R}
            fill="none"
            style={{
              stroke: CHAPTER,
              strokeDasharray: `${circumference} ${circumference}`,
              strokeDashoffset: circumference - done,
              transition: animate ? `stroke-dashoffset ${TRAVEL_MS}ms ${ease}` : "none",
            }}
            strokeOpacity="0.16"
            strokeWidth="7"
          />
          <circle
            cx={cx}
            cy={cy}
            r={R}
            fill="none"
            style={{
              stroke: CHAPTER,
              strokeDasharray: `${circumference} ${circumference}`,
              strokeDashoffset: circumference - done,
              transition: animate ? `stroke-dashoffset ${TRAVEL_MS}ms ${ease}` : "none",
            }}
            strokeWidth="2"
            strokeLinecap="round"
          />
        </g>

        {/* Which way the loop turns. */}
        {STATIONS.map((item, index) => {
          const angle = toRad(stationAngle(index) + STEP_DEG / 2);
          const px = cx + R * Math.cos(angle);
          const py = cy + R * Math.sin(angle);
          const tx = -Math.sin(angle);
          const ty = Math.cos(angle);
          const nx = Math.cos(angle);
          const ny = Math.sin(angle);
          const tip = `${px + tx * 3.5} ${py + ty * 3.5}`;
          const left = `${px - tx * 2.5 + nx * 3.2} ${py - ty * 2.5 + ny * 3.2}`;
          const right = `${px - tx * 2.5 - nx * 3.2} ${py - ty * 2.5 - ny * 3.2}`;
          return (
            <path
              key={`chevron-${item.id}`}
              d={`M ${left} L ${tip} L ${right}`}
              fill="none"
              stroke="rgba(221,214,254,0.42)"
              strokeWidth="1.2"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          );
        })}

        {/* The marker. It rides the orbit under the stations, so at rest only its glow shows round the one it reached. */}
        <g
          style={{
            transform: `rotate(${turn * STEP_DEG}deg)`,
            transformOrigin: `${cx}px ${cy}px`,
            transformBox: "view-box",
            transition: animate ? `transform ${TRAVEL_MS}ms ${ease}` : "none",
          }}
        >
          {Array.from({ length: 8 }, (_, index) => {
            const near = -90 - direction * index * 4;
            const far = -90 - direction * (index + 1) * 4;
            return (
              <path
                key={index}
                d={direction > 0 ? arcPath(cx, cy, R, far, near) : arcPath(cx, cy, R, near, far)}
                fill="none"
                style={{ stroke: CHAPTER, transition: "opacity 400ms ease" }}
                strokeWidth={3.4 - index * 0.3}
                strokeLinecap="round"
                opacity={moving ? 0.8 * (1 - index / 8) : 0}
              />
            );
          })}
          <circle cx={cx} cy={cy - R} r={30} fill={`url(#${uid}-glow)`} />
          <circle cx={cx} cy={cy - R} r={6.5} fill="none" style={{ stroke: CHAPTER }} strokeWidth="1.5" />
          <circle cx={cx} cy={cy - R} r={3.2} fill="#fff" />
        </g>

        {/* The hub: where the session is, and the question the agent is asking. */}
        <g key={`hub-${active}`} className={animate ? "type-in" : undefined}>
          <text x={cx} y={hubTop} textAnchor="middle" className="mono" fontSize="9.5" letterSpacing="2.2" fill="#77839c">
            SESSION {session} · {pad(active + 1)} / {pad(N)}
          </text>
          {station.hub.map((line, index) => (
            <text
              key={line}
              x={cx}
              y={hubTop + 26 + index * (hubFont + 4)}
              textAnchor="middle"
              fontSize={hubFont}
              fontWeight="800"
              letterSpacing="-0.3"
              fill="#fff"
            >
              {line}
            </text>
          ))}
          <text x={cx} y={hubTop + hubSpan} textAnchor="middle" className="mono" fontSize="11.5" style={{ fill: colour }}>
            {commandName(station)}
          </text>
        </g>
      </g>

      {/* The stations, as tabs: arrow keys move between them. */}
      <g role="tablist" aria-label="The stations of one session">
        {g.nodes.map((node, index) => {
          const item = stationAt(index);
          const on = index === active;
          const passed = index < active;
          const tint = EFFECT_COLOUR[item.effect];
          const Icon = ICONS[item.id] ?? Hammer;
          return (
            <g
              key={item.id}
              ref={(element) => {
                tabs.current[index] = element;
              }}
              id={`${uid}-tab-${index}`}
              role="tab"
              aria-selected={on}
              aria-controls={`${uid}-panel-${index}`}
              aria-label={`${pad(index + 1)}, ${rowName(item)}: ${item.question}`}
              tabIndex={on ? 0 : -1}
              onClick={() => onSelect(index)}
              onKeyDown={(event) => onKey(event, index)}
              className="group cursor-pointer outline-none"
            >
              <circle cx={node.x} cy={node.y} r={g.nodeR + 11} fill="transparent" />
              {on ? (
                <circle
                  cx={node.x}
                  cy={node.y}
                  r={g.nodeR + 8}
                  fill="none"
                  style={{ stroke: tint }}
                  strokeOpacity="0.45"
                  strokeWidth="1.2"
                  className={pulse ? "pulse-dot" : undefined}
                />
              ) : null}
              <circle
                cx={node.x}
                cy={node.y}
                r={on ? g.nodeR + 2 : g.nodeR}
                fill="#07060f"
                style={{ stroke: tint }}
                strokeOpacity={on || passed ? 1 : 0.5}
                strokeWidth={on ? 2 : 1.3}
                strokeDasharray={item.effect === "harness" ? "3 3" : undefined}
              />
              {on ? <circle cx={node.x} cy={node.y} r={g.nodeR + 2} style={{ fill: tint }} fillOpacity="0.18" /> : null}
              <Icon
                x={node.x - 8}
                y={node.y - 8}
                size={16}
                strokeWidth={1.9}
                aria-hidden="true"
                style={{ color: on ? "#ffffff" : tint, opacity: on || passed ? 1 : 0.72 }}
              />
              <text
                x={node.nameX}
                y={node.nameY}
                textAnchor={node.anchor}
                className="mono transition-colors group-hover:fill-white"
                fontSize="12"
                fontWeight={on ? 700 : 500}
                fill={on ? "#ffffff" : "#cbd5e1"}
              >
                {item.name}
              </text>
              <text
                x={node.nameX}
                y={node.tagY}
                textAnchor={node.anchor}
                className="mono"
                fontSize="8.5"
                letterSpacing="1.2"
                style={{ fill: tint }}
                opacity={on ? 1 : 0.7}
              >
                {item.tag.toUpperCase()}
              </text>
              <circle
                cx={node.x}
                cy={node.y}
                r={g.nodeR + 5}
                fill="none"
                stroke="#a78bfa"
                strokeWidth="2"
                className="opacity-0 group-focus-visible:opacity-100"
              />
            </g>
          );
        })}
      </g>
    </svg>
  );
}

function Legend() {
  return (
    <ul className="flex flex-wrap gap-x-4 gap-y-1.5 font-mono text-[10.5px] text-slate-400" aria-label="Key">
      <li className="flex items-center gap-2">
        <span aria-hidden="true" className="h-[3px] w-5 rounded-full" style={{ background: CHAPTER }} />
        reads only
      </li>
      <li className="flex items-center gap-2">
        <span aria-hidden="true" className="h-[3px] w-5 rounded-full" style={{ background: WRITE }} />
        writes
      </li>
      <li className="flex items-center gap-2">
        <span aria-hidden="true" className="w-5 border-t-2 border-dashed" style={{ borderColor: HARNESS }} />
        the harness
      </li>
      <li className="flex items-center gap-2">
        <span aria-hidden="true" className="w-5 border-t border-dashed border-slate-500" />
        between sessions
      </li>
    </ul>
  );
}

/** A hairline that fills while the figure rests on a station, so the reader can see when it will move on. */
function Countdown({ running, restart }: { running: boolean; restart: number }) {
  const line = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    const element = line.current;
    if (!element || !running || typeof element.animate !== "function") return;
    const animation = element.animate([{ transform: "scaleX(0)" }, { transform: "scaleX(1)" }], {
      duration: STEP_MS,
      easing: "linear",
      fill: "forwards",
    });
    return () => animation.cancel();
  }, [running, restart]);
  return (
    <span
      ref={line}
      aria-hidden="true"
      className="pointer-events-none absolute inset-x-5 top-0 h-[2px] origin-left rounded-full bg-[color:var(--chapter,#a3e635)]/70"
      style={{ transform: "scaleX(0)" }}
    />
  );
}

/** A phone's version: the stations as a vertical rail, the one in focus opened in place. */
function Stepper({
  active,
  session,
  running,
  animate,
  pulse,
  uid,
  controls,
  onSelect,
}: {
  active: number;
  session: number;
  running: boolean;
  animate: boolean;
  pulse: boolean;
  uid: string;
  controls: ReactNode;
  onSelect: (index: number) => void;
}) {
  const heads = useRef<(HTMLButtonElement | null)[]>([]);
  // Opening a station closes the one above it, which can lift the tapped row off the top of the screen
  // (Safari keeps no scroll anchor). Once the list has re-rendered, bring that row back into view.
  const pick = (index: number) => {
    onSelect(index);
    window.requestAnimationFrame(() =>
      heads.current[index]?.scrollIntoView({ block: "nearest", behavior: animate ? "smooth" : "auto" }),
    );
  };
  return (
    <div className="mt-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="font-mono text-[10.5px] tracking-[0.16em] text-slate-500 uppercase">
          Session {session} · {pad(active + 1)} / {pad(N)}
        </p>
        {controls}
      </div>
      <ol className="mt-4" aria-label="The stations of one session">
        {STATIONS.map((station, index) => {
          const on = index === active;
          const passed = index < active;
          const tint = EFFECT_COLOUR[station.effect];
          const Icon = ICONS[station.id] ?? Hammer;
          return (
            <li key={station.id} className="grid grid-cols-[2.25rem_minmax(0,1fr)] gap-x-3">
              <div className="flex flex-col items-center" aria-hidden="true">
                <span
                  className={cn(
                    "h-2.5 w-[2px] transition-colors duration-500",
                    index === 0 ? "bg-transparent" : on || passed ? "bg-[color:var(--chapter,#a3e635)]" : "bg-white/10",
                  )}
                />
                <span
                  className="relative flex h-9 w-9 shrink-0 items-center justify-center rounded-full border bg-[#07060f]"
                  style={{
                    borderColor: tint,
                    borderStyle: station.effect === "harness" ? "dashed" : "solid",
                    opacity: on || passed ? 1 : 0.6,
                    boxShadow: on ? `0 0 18px -2px color-mix(in srgb, ${tint} 70%, transparent)` : undefined,
                  }}
                >
                  {on ? (
                    <span
                      className={cn("absolute -inset-1.5 rounded-full border", pulse && "pulse-dot")}
                      style={{ borderColor: `color-mix(in srgb, ${tint} 50%, transparent)` }}
                    />
                  ) : null}
                  <Icon className="h-4 w-4" style={{ color: on ? "#ffffff" : tint }} />
                </span>
                <span
                  className={cn(
                    "w-[2px] flex-1 transition-colors duration-500",
                    passed ? "bg-[color:var(--chapter,#a3e635)]" : "bg-white/10",
                  )}
                />
              </div>
              <div className="min-w-0 pb-4">
                <button
                  ref={(element) => {
                    heads.current[index] = element;
                  }}
                  type="button"
                  aria-expanded={on}
                  // One station is always open, so the open one's header cannot close it.
                  aria-disabled={on || undefined}
                  aria-controls={`${uid}-step-${index}`}
                  onClick={() => pick(index)}
                  className="flex w-full flex-col items-start rounded-lg px-1 pt-2.5 text-left focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none"
                >
                  <span className="flex w-full flex-wrap items-baseline gap-x-2 gap-y-0.5">
                    <span className="font-mono text-[10.5px] text-slate-500 tabular-nums">{pad(index + 1)}</span>
                    <span className={cn("font-mono text-[14px] font-bold", on ? "text-white" : "text-slate-200")}>
                      {rowName(station)}
                    </span>
                    <span className="ml-auto text-[9px] font-black tracking-[0.14em] uppercase" style={{ color: tint }}>
                      {station.tag}
                    </span>
                  </span>
                  <span className="mt-0.5 text-[13.5px] leading-snug text-slate-400">{station.question}</span>
                </button>
                <div id={`${uid}-step-${index}`} hidden={!on} className="relative">
                  {on ? (
                    <div className={cn("mt-3 pt-3", animate && "type-in")}>
                      <Countdown running={running} restart={active} />
                      <StationDetail station={station} index={index} variant="compact" />
                    </div>
                  ) : null}
                </div>
              </div>
            </li>
          );
        })}
        <li className="grid grid-cols-[2.25rem_minmax(0,1fr)] gap-x-3">
          <div className="flex flex-col items-center" aria-hidden="true">
            <span className="h-2.5 w-0 border-l-2 border-dashed border-slate-600" />
            <span className="flex h-9 w-9 items-center justify-center rounded-full border border-dashed border-slate-500">
              <RotateCcw className="h-4 w-4 text-slate-400" />
            </span>
          </div>
          <button
            type="button"
            onClick={() => pick(0)}
            className="rounded-lg px-1 pt-2.5 text-left text-[13.5px] leading-snug text-slate-400 focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none"
          >
            <span className="font-mono text-[10.5px] tracking-[0.16em] text-slate-500 uppercase">Next session</span>
            <span className="block">
              Back to <span className="font-mono text-slate-200">ee resume</span> for the next session.
            </span>
          </button>
        </li>
      </ol>
    </div>
  );
}

/**
 * How a station's detail is laid out. `full` is one column of everything; `split` puts the JSON beside the
 * text, for a tablet's wide panel; `main` and `why` divide it between a wide screen's two columns, so the
 * space under the ring is used; `compact` is the phone stepper's, with no heading (the row above is one).
 */
type Variant = "full" | "split" | "main" | "why" | "compact";

/** One station's detail: the command, the paths an agent reads, what comes back, and why it is shaped that way. */
function StationDetail({
  station,
  index,
  variant,
  fixedHeight = false,
}: {
  station: Station;
  index: number;
  variant: Variant;
  fixedHeight?: boolean;
}) {
  const tint = EFFECT_COLOUR[station.effect];
  const files = [...new Set(station.sources.map((source) => source.file))];

  const head = (
    <>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="font-mono text-[11px] text-[color:var(--chapter,#a3e635)] tabular-nums">
          {pad(index + 1)} / {pad(N)}
        </span>
        <Heading level={5} className="text-lg font-extrabold tracking-[-0.02em] text-white">
          {station.title}
        </Heading>
        <span
          className="rounded-full border px-2 py-0.5 text-[10px] font-black tracking-[0.12em] uppercase"
          style={{ color: tint, borderColor: `color-mix(in srgb, ${tint} 40%, transparent)` }}
        >
          {station.chip}
        </span>
      </div>
      <p className="mt-1 text-[15px] leading-snug text-slate-300">{station.question}</p>
    </>
  );

  const command = (
    <>
      {station.command ? (
        <CommandLine command={station.command} />
      ) : (
        <div className="mt-3 rounded-xl border border-dashed border-white/[0.14] px-3 py-2.5 font-mono text-[12.5px] text-slate-400">
          <span className="text-slate-500 select-none">$ </span>
          <span className="italic">the harness&apos;s own tools: edit, build, test, ask for approval</span>
        </div>
      )}
      {station.note ? <p className="mt-2 font-mono text-[11px] leading-relaxed text-slate-500">{station.note}</p> : null}
    </>
  );

  const reads =
    station.reads.length > 0 ? (
      <dl className="mt-4">
        <dt className={LABEL}>The agent reads</dt>
        <dd className="mt-1.5 flex flex-wrap gap-1.5">
          {station.reads.map((path) => (
            <code
              key={path}
              className="rounded-md border border-[color:var(--chapter,#a3e635)]/25 bg-[color:var(--chapter,#a3e635)]/[0.07] px-1.5 py-0.5 font-mono text-[11.5px] [overflow-wrap:anywhere] text-[color:var(--chapter,#a3e635)]"
            >
              {path}
            </code>
          ))}
        </dd>
        {station.footnote ? <dd className="mt-1.5 text-[11.5px] leading-snug text-slate-500">{station.footnote}</dd> : null}
      </dl>
    ) : null;

  const why = (
    <dl className={cn("space-y-3.5", variant !== "why" && "mt-4")}>
      <div>
        <dt className={LABEL}>What it returns</dt>
        <dd className="mt-1 text-[13.5px] leading-relaxed text-slate-300">{station.returns}</dd>
      </div>
      <div className="border-l-2 pl-3" style={{ borderColor: tint }}>
        <dt className={LABEL}>Why it works this way</dt>
        <dd className="mt-1 text-[13.5px] leading-relaxed text-slate-200">{station.because}</dd>
      </div>
    </dl>
  );

  const checked = (
    <p className="mt-3 font-mono text-[10.5px] leading-relaxed [overflow-wrap:anywhere] text-slate-500">
      checked against {files.join(" · ")}
    </p>
  );

  const json = <JsonExcerpt station={station} fixedHeight={fixedHeight} collapsed={variant === "compact"} />;

  if (variant === "why") {
    return (
      <div className="min-w-0">
        {why}
        {checked}
      </div>
    );
  }
  if (variant === "split") {
    return (
      <div className="min-w-0">
        {head}
        {command}
        <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.08fr)] gap-x-6">
          <div className="min-w-0">
            {reads}
            {why}
            {checked}
          </div>
          <div className="min-w-0">{json}</div>
        </div>
      </div>
    );
  }
  return (
    <div className="min-w-0">
      {variant === "compact" ? null : head}
      {command}
      {reads}
      {variant === "main" ? null : why}
      {json}
      {variant === "main" ? null : checked}
    </div>
  );
}

/**
 * Every station's detail in one grid cell, only the active one visible: the cell is as tall as the
 * tallest, so nothing below it jumps as the session moves. As tab panels, the ring's stations control them.
 */
function Crossfade({
  active,
  animate,
  uid,
  panels,
  render,
}: {
  active: number;
  animate: boolean;
  uid: string;
  panels: boolean;
  render: (station: Station, index: number) => ReactNode;
}) {
  return (
    <div className="grid">
      {STATIONS.map((station, index) => {
        const on = index === active;
        return (
          <div
            key={station.id}
            id={panels ? `${uid}-panel-${index}` : undefined}
            role={panels ? "tabpanel" : undefined}
            aria-labelledby={panels ? `${uid}-tab-${index}` : undefined}
            aria-hidden={!on}
            className="min-w-0 [grid-area:1/1]"
            style={{
              opacity: on ? 1 : 0,
              visibility: on ? "visible" : "hidden",
              transition: animate
                ? on
                  ? "opacity 320ms ease, visibility 0s linear 0s"
                  : "opacity 220ms ease, visibility 0s linear 220ms"
                : "none",
            }}
          >
            {render(station, index)}
          </div>
        );
      })}
    </div>
  );
}

const COMMAND_CLASS: Record<CommandToken["kind"], string> = {
  bin: "font-bold text-white",
  sub: "text-[color:var(--chapter,#a3e635)]",
  flag: "text-sky-200/90",
  str: "text-amber-200/90",
  hole: "italic text-slate-400",
  arg: "text-slate-200",
};

function CommandLine({ command }: { command: string }) {
  const [copied, setCopied] = useState(0);
  // Each copy restarts the check mark's time; the timer goes with the component.
  useEffect(() => {
    if (copied === 0) return;
    const timer = window.setTimeout(() => setCopied(0), 1600);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied((count) => count + 1);
    } catch {
      // No clipboard (an insecure context, say): the command is still there to select.
    }
  };
  return (
    <div className="mt-3 flex items-start gap-2 rounded-xl border border-white/[0.08] bg-black/40 py-2 pr-2 pl-3">
      <pre className="min-w-0 flex-1 py-0.5 font-mono text-[12.5px] leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere]">
        <code>
          <span className="text-slate-500 select-none">$ </span>
          {commandTokens(command).map((token, index) => (
            <Fragment key={index}>
              {index > 0 ? " " : null}
              <span className={COMMAND_CLASS[token.kind]}>{token.text}</span>
            </Fragment>
          ))}
        </code>
      </pre>
      <button
        type="button"
        onClick={copy}
        aria-label={copied ? "Copied" : "Copy the command"}
        className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-white/10 text-slate-400 transition hover:border-[color:var(--chapter,#a3e635)]/50 hover:text-white focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none"
      >
        {copied ? <Check className="h-3.5 w-3.5" aria-hidden="true" /> : <Copy className="h-3.5 w-3.5" aria-hidden="true" />}
      </button>
      <span className="sr-only" aria-live="polite">
        {copied ? "Copied to the clipboard" : ""}
      </span>
    </div>
  );
}

const JSON_CLASS: Record<JsonToken["kind"], string> = {
  key: "text-sky-200/85",
  string: "text-amber-100/80",
  literal: "text-violet-200",
  elided: "text-slate-500",
  punct: "text-slate-500",
  space: "",
  other: "text-slate-300",
};

/** 11 px at a line height of 1.6, and the box's own padding. */
const JSON_LINE_PX = 17.6;

function JsonExcerpt({
  station,
  fixedHeight,
  collapsed = false,
}: {
  station: Station;
  fixedHeight: boolean;
  /** A phone's stepper: the excerpt is a tap away, so one open station does not run to three screens. */
  collapsed?: boolean;
}) {
  const minHeight = fixedHeight ? MAX_JSON_LINES * JSON_LINE_PX + 18 : undefined;
  const label = (
    <span className={LABEL}>
      What comes back{station.case ? <span className="tracking-[0.12em] normal-case"> · {station.case}</span> : null}
    </span>
  );
  const body = station.json ? (
    <pre
      className="mt-1.5 overflow-x-auto rounded-xl border border-white/[0.07] bg-[#05040d]/70 py-2 font-mono text-[11px] leading-[1.6] [scrollbar-width:thin]"
      style={{ minHeight }}
    >
      <code className="block min-w-max">
        {station.json.map((line, index) => (
          <span
            key={index}
            className={cn(
              "block border-l-2 pr-3 pl-2.5",
              line.hot
                ? "border-[color:var(--chapter,#a3e635)] bg-[color:var(--chapter,#a3e635)]/[0.07]"
                : "border-transparent",
            )}
          >
            {jsonTokens(line.text).map((token, at) => (
              <span
                key={at}
                className={
                  token.kind === "key" && line.hot
                    ? "font-semibold text-[color:var(--chapter,#a3e635)]"
                    : JSON_CLASS[token.kind]
                }
              >
                {token.text}
              </span>
            ))}
          </span>
        ))}
      </code>
    </pre>
  ) : (
    <div
      className="mt-1.5 flex items-center justify-center rounded-xl border border-dashed border-white/[0.12] px-5 py-6 text-center text-[13px] leading-relaxed text-slate-400"
      style={{ minHeight }}
    >
      No ee response at this step. The harness does the work with its own tools and calls ee again when there is
      something to record.
    </div>
  );

  if (collapsed && station.json) {
    return (
      <details className="group mt-4">
        <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-2 gap-y-0.5 rounded-md py-1 focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none [&::-webkit-details-marker]:hidden">
          <ChevronRight
            className="h-3 w-3 text-slate-500 transition-transform group-open:rotate-90"
            aria-hidden="true"
          />
          {label}
          <span className="ml-auto font-mono text-[10px] text-slate-500">JSON excerpt · {station.json.length} lines</span>
        </summary>
        {body}
        <p className="mt-1.5 font-mono text-[10px] text-slate-500">highlighted: what the agent reads</p>
      </details>
    );
  }

  return (
    <div className="mt-4">
      <p className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
        {label}
        <span className="font-mono text-[10px] text-slate-500">excerpt · highlighted: what the agent reads</span>
      </p>
      {body}
    </div>
  );
}
