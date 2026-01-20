import { memo } from "react";

interface ActivityItemProps {
  type: "Sent" | "Received";
  address: string;
  amount: string;
  time: string;
}

const ActivityItem = memo(({ type, address, amount, time }: ActivityItemProps) => {
  const isSent = type === "Sent";

  return (
    <article className="flex items-center gap-4 p-4 mb-2 bg-dark-surfaceContainerLow rounded-2xl hover:bg-dark-surfaceContainer transition-colors group">
      {/* Transaction Icon Container */}
      <div className={`w-12 h-12 rounded-full flex items-center justify-center 
        ${isSent ? 'bg-dark-surfaceVariant text-dark-onSurfaceVariant' : 'bg-dark-primaryContainer text-dark-onPrimaryContainer'}`}>
        {isSent ? (
          <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="size-5">
            <path strokeLinecap="round" strokeLinejoin="round" d="M4.5 19.5 19.5 4.5m0 0H8.25m11.25 0v11.25" />
          </svg>
        ) : (
          <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="size-5">
            <path strokeLinecap="round" strokeLinejoin="round" d="m19.5 4.5-15 15m0 0h11.25m-11.25 0V8.25" />
          </svg>
        )}
      </div>

      {/* Details */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="font-bold text-dark-onSurface">{type}</span>
          <span className="text-xs text-dark-outline uppercase font-medium">{time}</span>
        </div>
        <code className="text-sm text-dark-onSurfaceVariant truncate block mt-0.5 opacity-80 group-hover:opacity-100 transition-opacity">
          {address}
        </code>
      </div>

      {/* Amount */}
      <div className="text-right">
        <span className={`font-bold ${isSent ? 'text-dark-onSurface' : 'text-dark-primary'}`}>
          {isSent ? "-" : "+"}{amount}
        </span>
        <span className="text-[10px] block text-dark-outline font-bold">WISP</span>
      </div>
    </article>
  );
});

export default ActivityItem;