import { cronJobs } from "convex/server";
import { internal } from "./_generated/api";

const crons = cronJobs();

// GitHub keeps repo traffic for 14 days; save it daily (githubTraffic.ts).
crons.daily("save GitHub traffic", { hourUTC: 6, minuteUTC: 0 }, internal.githubTraffic.fetchTraffic, {});

export default crons;
