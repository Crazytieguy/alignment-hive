import { createFileRoute } from "@tanstack/react-router";
import { type FormEvent, useEffect, useMemo, useState } from "react";
import { DateTime } from "luxon";
import { Button } from "@alignment-hive/ui";
import { ensureBotId } from "@/lib/booking/botid-client";
import { BookingShell } from "@/components/booking/booking-shell";
import {
  DURATIONS,
  type Duration,
  OFFICES,
  type OfficeConfig,
  type OfficeSlug,
  isOfficeSlug,
} from "@/lib/booking/offices";
import { type BusyInterval, type Slot, generateSlots } from "@/lib/booking/slots";

export const Route = createFileRoute("/book/$office")({
  component: BookOfficeRoute,
});

function BookOfficeRoute() {
  const { office } = Route.useParams();
  if (!isOfficeSlug(office)) {
    return (
      <BookingShell>
        <h1 className="text-2xl font-semibold text-slate-900 dark:text-slate-100">
          Office not found
        </h1>
        <p className="mt-2 text-slate-600 dark:text-slate-400">
          <a className="underline" href="/book">
            See the offices you can book.
          </a>
        </p>
      </BookingShell>
    );
  }
  return <Booking office={office} />;
}

const inputClass =
  "w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-900 px-3 py-2 text-slate-900 dark:text-slate-100";

function fmtTime(ms: number, zone: string): string {
  return DateTime.fromMillis(ms, { zone }).toFormat("h:mm a ZZZZ");
}
function fmtDate(ms: number, zone: string): string {
  return DateTime.fromMillis(ms, { zone }).toFormat("cccc, LLLL d");
}
/** Visitor-tz time, plus the office time when the visitor isn't already on office time. */
function timeLabel(ms: number, visitorZone: string, officeZone: string): string {
  const visitor = fmtTime(ms, visitorZone);
  const office = fmtTime(ms, officeZone);
  return visitor === office ? visitor : `${visitor} (${office} at the office)`;
}

/** Suggested length by group size: 60 min solo, 90 for 2–3, 120 for 4+. */
function suggestedDuration(attendees: number): Duration {
  return attendees >= 4 ? 120 : attendees >= 2 ? 90 : 60;
}

interface Availability {
  busy: BusyInterval[];
  nowUtc: number;
}

async function fetchBusy(office: string): Promise<Availability> {
  await ensureBotId();
  const res = await fetch("/booking/availability", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ office }),
  });
  const data = (await res.json().catch(() => null)) as
    | { busy?: BusyInterval[]; nowUtc?: number; error?: string }
    | null;
  if (!res.ok) throw new Error(data?.error ?? "Couldn't load availability.");
  return { busy: data?.busy ?? [], nowUtc: data?.nowUtc ?? Date.now() };
}

function Booking({ office }: { office: OfficeSlug }) {
  const config = OFFICES[office];
  const visitorZone = useMemo(
    () => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    [],
  );

  const [avail, setAvail] = useState<Availability | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [duration, setDuration] = useState<Duration>(60);
  const [selectedDate, setSelectedDate] = useState<string | null>(null);
  const [selectedStartUtc, setSelectedStartUtc] = useState<number | null>(null);
  const [cancelUrl, setCancelUrl] = useState<string | null>(null);

  // Fetch the host's busy intervals once. Availability is duration-independent, so switching
  // duration just recomputes start times locally — no refetch.
  useEffect(() => {
    let cancelled = false;
    setAvail(null);
    setLoadError(null);
    fetchBusy(office)
      .then((next) => !cancelled && setAvail(next))
      .catch((err: Error) => !cancelled && setLoadError(err.message));
    return () => {
      cancelled = true;
    };
  }, [office]);

  const byDate = useMemo(() => {
    const map = new Map<string, Slot[]>();
    if (!avail) return map;
    for (const slot of generateSlots(config, duration, avail.busy, avail.nowUtc)) {
      const key = DateTime.fromMillis(slot.startUtc, { zone: config.timezone }).toISODate() ?? "";
      const arr = map.get(key);
      if (arr) arr.push(slot);
      else map.set(key, [slot]);
    }
    return map;
  }, [avail, duration, config]);

  const dates = useMemo(() => [...byDate.keys()], [byDate]);
  const times = selectedDate ? (byDate.get(selectedDate) ?? []) : [];

  // Keep the date selection valid as availability/duration change.
  useEffect(() => {
    if (dates.length === 0) {
      if (selectedDate !== null) setSelectedDate(null);
    } else if (!selectedDate || !dates.includes(selectedDate)) {
      setSelectedDate(dates[0]);
    }
  }, [dates, selectedDate]);

  // The set of times changes with the date or duration, so clear any stale time pick.
  useEffect(() => {
    setSelectedStartUtc(null);
  }, [selectedDate, duration]);

  const selectedSlot = times.find((t) => t.startUtc === selectedStartUtc) ?? null;

  if (cancelUrl && selectedSlot) {
    return (
      <Confirmation
        config={config}
        slot={selectedSlot}
        duration={duration}
        visitorZone={visitorZone}
        cancelUrl={cancelUrl}
      />
    );
  }

  return (
    <BookingShell>
      <a
        className="text-sm text-slate-500 underline hover:text-slate-700 dark:hover:text-slate-300"
        href="/book"
      >
        ← All offices
      </a>
      <h1 className="mt-3 text-3xl font-serif font-bold text-slate-900 dark:text-slate-100">
        Book a consulting session with Yoav
      </h1>
      <p className="mt-1 text-slate-600 dark:text-slate-400">In-person at {config.label}.</p>

      {loadError ? (
        <p className="mt-6 text-red-600 dark:text-red-400">{loadError}</p>
      ) : !avail ? (
        <p className="mt-6 text-slate-500">Loading availability…</p>
      ) : (
        <Form
          office={office}
          config={config}
          visitorZone={visitorZone}
          duration={duration}
          onDuration={setDuration}
          dates={dates}
          byDate={byDate}
          times={times}
          selectedDate={selectedDate}
          onDate={setSelectedDate}
          selectedStartUtc={selectedStartUtc}
          onTime={setSelectedStartUtc}
          selectedSlot={selectedSlot}
          onBooked={setCancelUrl}
          onSlotTaken={() => {
            setSelectedStartUtc(null);
            setAvail(null);
            fetchBusy(office)
              .then(setAvail)
              .catch(() => setAvail({ busy: [], nowUtc: Date.now() }));
          }}
        />
      )}
    </BookingShell>
  );
}

function Form({
  office,
  config,
  visitorZone,
  duration,
  onDuration,
  dates,
  byDate,
  times,
  selectedDate,
  onDate,
  selectedStartUtc,
  onTime,
  selectedSlot,
  onBooked,
  onSlotTaken,
}: {
  office: OfficeSlug;
  config: OfficeConfig;
  visitorZone: string;
  duration: Duration;
  onDuration: (d: Duration) => void;
  dates: string[];
  byDate: Map<string, Slot[]>;
  times: Slot[];
  selectedDate: string | null;
  onDate: (d: string) => void;
  selectedStartUtc: number | null;
  onTime: (ms: number | null) => void;
  selectedSlot: Slot | null;
  onBooked: (cancelUrl: string) => void;
  onSlotTaken: () => void;
}) {
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [participants, setParticipants] = useState<string[]>([]);
  const [note, setNote] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [durationTouched, setDurationTouched] = useState(false);

  // Default the length to the group size until the visitor picks one themselves.
  const attendees = 1 + participants.filter((p) => p.trim()).length;
  useEffect(() => {
    if (!durationTouched) onDuration(suggestedDuration(attendees));
  }, [attendees, durationTouched, onDuration]);

  async function submit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (submitting || !selectedSlot) return; // guard against double-submit
    setSubmitting(true);
    setError(null);
    try {
      await ensureBotId();
      const res = await fetch("/booking/create", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          office,
          durationMin: duration,
          slotStartUtc: selectedSlot.startUtc,
          name,
          email,
          participants: participants.map((p) => p.trim()).filter(Boolean),
          note,
        }),
      });
      if (res.status === 409) {
        onSlotTaken();
        setError("That time was just taken — please pick another.");
        return;
      }
      const data = (await res.json().catch(() => null)) as
        | { cancelUrl?: string; error?: string }
        | null;
      if (!res.ok || !data?.cancelUrl) {
        setError(data?.error ?? "Something went wrong. Please try again.");
        return;
      }
      onBooked(data.cancelUrl);
    } catch {
      setError("Network error. Please try again.");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={submit} className="mt-6 space-y-5">
      {/* Duration */}
      <div>
        <span className="block text-sm font-medium text-slate-700 dark:text-slate-300">
          Meeting length
        </span>
        <p className="mt-0.5 text-xs text-slate-500 dark:text-slate-400">
          We suggest 60 min for one person, 90 for 2–3, and 120 for 4+ — but pick whatever works.
        </p>
        <div className="mt-2 flex gap-2">
          {DURATIONS.map((d) => (
            <button
              key={d}
              type="button"
              onClick={() => {
                setDurationTouched(true);
                onDuration(d);
              }}
              className={
                d === duration
                  ? "rounded-md bg-slate-900 px-4 py-2 text-sm font-medium text-white dark:bg-slate-100 dark:text-slate-900"
                  : "rounded-md border border-slate-300 dark:border-slate-700 px-4 py-2 text-sm font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800"
              }
            >
              {d} min
            </button>
          ))}
        </div>
      </div>

      {dates.length === 0 ? (
        <p className="text-slate-500">No open times in the next few weeks.</p>
      ) : (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <label className="block">
            <span className="text-sm font-medium text-slate-700 dark:text-slate-300">Date</span>
            <select
              className={inputClass}
              value={selectedDate ?? ""}
              onChange={(e) => onDate(e.target.value)}
            >
              {dates.map((d) => (
                <option key={d} value={d}>
                  {fmtDate(byDate.get(d)![0].startUtc, config.timezone)}
                </option>
              ))}
            </select>
          </label>
          <label className="block">
            <span className="text-sm font-medium text-slate-700 dark:text-slate-300">Time</span>
            <select
              className={inputClass}
              value={selectedStartUtc ?? ""}
              onChange={(e) => onTime(e.target.value ? Number(e.target.value) : null)}
            >
              <option value="">Select a time…</option>
              {times.map((t) => (
                <option key={t.startUtc} value={t.startUtc}>
                  {timeLabel(t.startUtc, visitorZone, config.timezone)}
                </option>
              ))}
            </select>
          </label>
        </div>
      )}

      <p className="text-sm text-slate-500 dark:text-slate-400">
        Times are shown in your timezone ({visitorZone}); the office is on Pacific time.
      </p>

      <hr className="border-slate-200 dark:border-slate-800" />

      <label className="block">
        <span className="text-sm font-medium text-slate-700 dark:text-slate-300">Your name</span>
        <input className={inputClass} value={name} onChange={(e) => setName(e.target.value)} required />
      </label>
      <label className="block">
        <span className="text-sm font-medium text-slate-700 dark:text-slate-300">Your email</span>
        <input
          className={inputClass}
          type="email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          required
        />
      </label>

      {/* Additional participants — optional group booking */}
      <div>
        <span className="text-sm font-medium text-slate-700 dark:text-slate-300">Participants</span>
        <p className="mt-0.5 text-xs text-slate-500 dark:text-slate-400">
          By default this is a 1:1 with Yoav. Booking as a group? Add the others' emails and they'll be
          invited too.
        </p>
        <div className="mt-2 space-y-2">
          {participants.map((p, i) => (
            <div key={i} className="flex gap-2">
              <input
                className={inputClass}
                type="email"
                placeholder="participant@email.com"
                value={p}
                onChange={(e) =>
                  setParticipants(participants.map((v, j) => (j === i ? e.target.value : v)))
                }
              />
              <Button
                type="button"
                variant="outline"
                onClick={() => setParticipants(participants.filter((_, j) => j !== i))}
              >
                Remove
              </Button>
            </div>
          ))}
        </div>
        <button
          type="button"
          onClick={() => setParticipants([...participants, ""])}
          className="mt-2 text-sm font-medium text-slate-700 underline dark:text-slate-300"
        >
          + Add participant
        </button>
      </div>

      <label className="block">
        <span className="text-sm font-medium text-slate-700 dark:text-slate-300">Notes (optional)</span>
        <textarea
          className={inputClass}
          rows={3}
          placeholder="Anything you'd like to share ahead of time."
          value={note}
          onChange={(e) => setNote(e.target.value)}
        />
      </label>

      {error && <p className="text-sm text-red-600 dark:text-red-400">{error}</p>}
      <Button type="submit" disabled={submitting || !selectedSlot}>
        {submitting ? "Booking…" : "Confirm booking"}
      </Button>
    </form>
  );
}

function Confirmation({
  config,
  slot,
  duration,
  visitorZone,
  cancelUrl,
}: {
  config: OfficeConfig;
  slot: Slot;
  duration: Duration;
  visitorZone: string;
  cancelUrl: string;
}) {
  return (
    <BookingShell>
      <h1 className="text-3xl font-serif font-bold text-slate-900 dark:text-slate-100">
        You're booked
      </h1>
      <p className="mt-2 text-slate-700 dark:text-slate-300">
        {fmtDate(slot.startUtc, config.timezone)} · {fmtTime(slot.startUtc, visitorZone)} ({duration}{" "}
        min) · {config.label}
      </p>
      <p className="mt-4 text-slate-600 dark:text-slate-400">
        Calendar invites are on their way to everyone's email.
      </p>
      <p className="mt-6 text-sm text-slate-600 dark:text-slate-400">
        Need to cancel?{" "}
        <a className="underline" href={cancelUrl}>
          Cancel this booking
        </a>
        .
      </p>
    </BookingShell>
  );
}
