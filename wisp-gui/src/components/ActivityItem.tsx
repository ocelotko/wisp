import { memo } from "react";

interface ActivityItemProps {
  tx: any;
}

const ActivityItem = memo(({ tx }: ActivityItemProps) => {
  let netChange = 0;

  // 1. Calculate value using 'is_ours' flags from enriched Rust data
  tx.transaction.outputs.forEach((out: any) => {
    if (out.is_ours) netChange += out.value;
  });

  tx.transaction.inputs.forEach((input: any) => {
    if (input.previous_output?.is_ours) {
      netChange -= input.previous_output.value;
    }
  });

  const isReceived = netChange > 0;
  const isSelf = netChange === 0;
  const absAmount = Math.abs(netChange / 100_000_000);

  const displayAmount = absAmount.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 8,
  });

  // 2. CRITICAL FIX: Extract string from Rust enum object to prevent black screen
  const statusRaw = tx.status;
  const statusString =
    typeof statusRaw === "string" ? statusRaw : Object.keys(statusRaw)[0];

  const isConfirmed = statusString === "Confirmed";

  // --- Static Icons ---
  const ReceiveIcon = (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      fill="none"
      viewBox="0 0 24 24"
      strokeWidth={1.5}
      stroke="currentColor"
      className="size-5"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="m19.5 4.5-15 15m0 0h11.25m-11.25 0V8.25"
      />
    </svg>
  );

  const SendIcon = (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      fill="none"
      viewBox="0 0 24 24"
      strokeWidth={1.5}
      stroke="currentColor"
      className="size-5"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M6 12L18 12M18 12L12.75 6.75M18 12L12.75 17.25"
      />
    </svg>
  );

  const SelfIcon = (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      fill="none"
      viewBox="0 0 24 24"
      strokeWidth={1.5}
      stroke="currentColor"
      className="size-5"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182m0-4.991v4.99"
      />
    </svg>
  );

  // --- Dynamic Config ---
  const config = isSelf
    ? { label: "Self", color: "text-blue-400", icon: SelfIcon, sign: "" }
    : isReceived
      ? {
          label: "Received",
          color: "text-green-400",
          icon: ReceiveIcon,
          sign: "+",
        }
      : { label: "Sent", color: "text-red-400", icon: SendIcon, sign: "-" };

  return (
    <div className="flex items-center justify-between p-4 bg-dark-surfaceContainerLow rounded-2xl border border-dark-outlineVariant mb-2 hover:border-dark-primary/30 hover:bg-dark-surfaceContainer transition-all active:scale-[0.99] group">
      <div className="flex items-center gap-4">
        <div
          className={`p-3 rounded-full transition-colors ${
            isReceived
              ? "bg-green-500/10 text-green-500"
              : isSelf
                ? "bg-blue-500/10 text-blue-400"
                : "bg-red-500/10 text-red-500"
          }`}
        >
          {config.icon}
        </div>
        <div>
          <p className="font-bold text-dark-onSurface leading-tight">
            {config.label}
          </p>
          <p className="text-[10px] text-dark-outline uppercase font-mono tracking-tighter opacity-70">
            {tx.transaction.id?.substring(0, 24)}
          </p>
        </div>
      </div>
      <div className="text-right">
        <p className={`font-bold font-mono text-lg ${config.color}`}>
          {config.sign}
          {displayAmount}
        </p>
        <div className="flex items-center justify-end gap-1.5 mt-0.5">
          <span
            className={`size-1.5 rounded-full ${isConfirmed ? "bg-green-500" : "bg-yellow-500 animate-pulse"}`}
          />
          <p className="text-[10px] text-dark-outline uppercase font-bold tracking-widest">
            {statusString}
          </p>
        </div>
      </div>
    </div>
  );
});

export default ActivityItem;
