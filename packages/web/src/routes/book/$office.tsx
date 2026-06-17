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

interface Slot {
  startUtc: number;
  endUtc: number;
}

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

function fmtTime(ms: number, zone: string): string {
  return DateTime.fromMillis(ms, { zone }).toFormat("h:mm a ZZZZ");
}
function fmtDate(ms: number, zone: string): string {
  return DateTime.fromMillis(ms, { zone }).toFormat("cccc, LLLL d");
}

function Booking({ office }: { office: OfficeSlug }) {
  const config = OFFICES[office];
  const visitorZone = useMemo(
    () => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    [],
  );

  const [duration, setDuration] = useState<Duration>(90);
  const [slots, setSlots] = useState<Slot[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [selected, setSelected] = useState<Slot | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [cancelUrl, setCancelUrl] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    ensureBotId()
      .then(() =>
        fetch("/booking/availability", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ office, durationMin: duration }),
        }),
      )
      .then(async (res) => {
        const data = (await res.json().catch(() => null)) as { slots?: Slot[]; error?: string } | null;
        if (!res.ok) throw new Error(data?.error ?? "Couldn't load availability.");
        return data?.slots ?? [];
      })
      .then((next) => !cancelled && setSlots(next))
      .catch((err: Error) => !cancelled && setLoadError(err.message))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [office, duration, reloadKey]);

  if (cancelUrl && selected) {
    return (
      <Confirmation
        config={config}
        slot={selected}
        duration={duration}
        visitorZone={visitorZone}
        cancelUrl={cancelUrl}
      />
    );
  }

  return (
    <BookingShell>
      <a className="text-sm text-slate-500 underline hover:text-slate-700 dark:hover:text-slate-300" href="/book">
        ← All offices
      </a>
      <h1 className="mt-3 text-3xl font-serif font-bold text-slate-900 dark:text-slate-100">
        {config.label}
      </h1>
      <p className="mt-1 text-slate-600 dark:text-slate-400">In-person meeting with Yoav.</p>

      {selected ? (
        <BookingForm
          office={office}
          config={config}
          duration={duration}
          slot={selected}
          visitorZone={visitorZone}
          onClose={() => setSelected(null)}
          onBooked={setCancelUrl}
          onSlotTaken={() => {
            setSelected(null);
            setNotice("That time was just taken — here's the latest availability.");
            setReloadKey((k) => k + 1);
          }}
        />
      ) : (
        <>
          <DurationPicker value={duration} onChange={setDuration} />
          <p className="mt-4 text-sm text-slate-500 dark:text-slate-400">
            Times are shown in your timezone ({visitorZone}); the office is on Pacific time.
          </p>
          {notice && (
            <p className="mt-3 rounded-md bg-amber-50 dark:bg-amber-950/30 px-3 py-2 text-sm text-amber-800 dark:text-amber-200">
              {notice}
            </p>
          )}
          {loading ? (
            <p className="mt-6 text-slate-500">Loading availability…</p>
          ) : loadError ? (
            <p className="mt-6 text-red-600 dark:text-red-400">{loadError}</p>
          ) : slots && slots.length === 0 ? (
            <p className="mt-6 text-slate-500">No open times in the next few weeks.</p>
          ) : slots ? (
            <SlotList
              slots={slots}
              visitorZone={visitorZone}
              officeZone={config.timezone}
              onPick={(s) => {
                setNotice(null);
                setSelected(s);
              }}
            />
          ) : null}
        </>
      )}
    </BookingShell>
  );
}

function DurationPicker({ value, onChange }: { value: Duration; onChange: (d: Duration) => void }) {
  return (
    <div className="mt-6">
      <span className="block text-sm font-medium text-slate-700 dark:text-slate-300">
        Meeting length
      </span>
      <div className="mt-2 flex gap-2">
        {DURATIONS.map((d) => (
          <button
            key={d}
            type="button"
            onClick={() => onChange(d)}
            className={
              d === value
                ? "rounded-md bg-slate-900 px-4 py-2 text-sm font-medium text-white dark:bg-slate-100 dark:text-slate-900"
                : "rounded-md border border-slate-300 dark:border-slate-700 px-4 py-2 text-sm font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800"
            }
          >
            {d} min
          </button>
        ))}
      </div>
    </div>
  );
}

function SlotList({
  slots,
  visitorZone,
  officeZone,
  onPick,
}: {
  slots: Slot[];
  visitorZone: string;
  officeZone: string;
  onPick: (s: Slot) => void;
}) {
  const groups = useMemo(() => {
    const map = new Map<string, Slot[]>();
    for (const s of slots) {
      const key = DateTime.fromMillis(s.startUtc, { zone: visitorZone }).toISODate() ?? "";
      const arr = map.get(key);
      if (arr) arr.push(s);
      else map.set(key, [s]);
    }
    return [...map.values()];
  }, [slots, visitorZone]);

  return (
    <div className="mt-6 space-y-6">
      {groups.map((daySlots) => (
        <div key={daySlots[0].startUtc}>
          <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
            {fmtDate(daySlots[0].startUtc, visitorZone)}
          </h3>
          <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-3">
            {daySlots.map((s) => (
              <button
                key={s.startUtc}
                type="button"
                onClick={() => onPick(s)}
                className="rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-900 px-3 py-2 text-left hover:border-slate-900 dark:hover:border-slate-300"
              >
                <span className="block text-sm font-medium text-slate-900 dark:text-slate-100">
                  {fmtTime(s.startUtc, visitorZone)}
                </span>
                <span className="block text-xs text-slate-500">
                  {fmtTime(s.startUtc, officeZone)} at the office
                </span>
              </button>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function SlotSummary({
  config,
  slot,
  duration,
  visitorZone,
}: {
  config: OfficeConfig;
  slot: Slot;
  duration: Duration;
  visitorZone: string;
}) {
  return (
    <div className="rounded-lg border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 p-4">
      <p className="font-medium text-slate-900 dark:text-slate-100">{config.label}</p>
      <p className="text-slate-700 dark:text-slate-300">{fmtDate(slot.startUtc, visitorZone)}</p>
      <p className="text-slate-700 dark:text-slate-300">
        {fmtTime(slot.startUtc, visitorZone)} ({duration} min)
      </p>
      <p className="text-xs text-slate-500">{fmtTime(slot.startUtc, config.timezone)} at the office</p>
    </div>
  );
}

function BookingForm({
  office,
  config,
  duration,
  slot,
  visitorZone,
  onClose,
  onBooked,
  onSlotTaken,
}: {
  office: OfficeSlug;
  config: OfficeConfig;
  duration: Duration;
  slot: Slot;
  visitorZone: string;
  onClose: () => void;
  onBooked: (cancelUrl: string) => void;
  onSlotTaken: () => void;
}) {
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [note, setNote] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (submitting) return; // guard against double-submit
    setSubmitting(true);
    setError(null);
    try {
      await ensureBotId();
      const res = await fetch("/booking/create", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ office, durationMin: duration, slotStartUtc: slot.startUtc, name, email, note }),
      });
      if (res.status === 409) {
        onSlotTaken();
        return;
      }
      const data = (await res.json().catch(() => null)) as { cancelUrl?: string; error?: string } | null;
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

  const inputClass =
    "w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-900 px-3 py-2 text-slate-900 dark:text-slate-100";

  return (
    <form onSubmit={submit} className="mt-6 space-y-4">
      <SlotSummary config={config} slot={slot} duration={duration} visitorZone={visitorZone} />
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
      <label className="block">
        <span className="text-sm font-medium text-slate-700 dark:text-slate-300">
          What would you like to chat about? (optional)
        </span>
        <textarea
          className={inputClass}
          rows={3}
          value={note}
          onChange={(e) => setNote(e.target.value)}
        />
      </label>
      {error && <p className="text-sm text-red-600 dark:text-red-400">{error}</p>}
      <div className="flex gap-3">
        <Button type="submit" disabled={submitting}>
          {submitting ? "Booking…" : "Confirm booking"}
        </Button>
        <Button type="button" variant="outline" onClick={onClose} disabled={submitting}>
          Back
        </Button>
      </div>
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
      <p className="mt-2 text-slate-600 dark:text-slate-400">
        A calendar invite is on its way to your email.
      </p>
      <div className="mt-6">
        <SlotSummary config={config} slot={slot} duration={duration} visitorZone={visitorZone} />
      </div>
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
