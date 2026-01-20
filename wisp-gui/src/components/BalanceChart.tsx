import { AreaChart, Area, XAxis, Tooltip, ResponsiveContainer } from "recharts";

const BalanceChart = ({ data }: { data: any[] }) => {
  return (
    <div className="h-full w-full">
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart
          data={data}
          margin={{ top: 10, right: 10, left: 10, bottom: 0 }}
        >
          <defs>
            <linearGradient id="colorBalance" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="#D0BCFF" stopOpacity={0.3} />
              <stop offset="95%" stopColor="#D0BCFF" stopOpacity={0} />
            </linearGradient>
          </defs>

          <XAxis dataKey="timestamp" hide={true} />

          <Tooltip
            cursor={{ stroke: "#49454F", strokeWidth: 1 }}
            contentStyle={{
              backgroundColor: "#1C1B1F",
              border: "1px solid #49454F",
              borderRadius: "16px",
              boxShadow: "0 10px 15px -3px rgba(0, 0, 0, 0.5)",
            }}
            itemStyle={{ color: "#D0BCFF", fontWeight: "bold" }}
            labelFormatter={(unixTime) =>
              new Date(unixTime * 1000).toLocaleString()
            }
          />

          <Area
            type="monotone"
            dataKey="balance"
            stroke="#D0BCFF"
            strokeWidth={3}
            fillOpacity={1}
            fill="url(#colorBalance)"
            animationDuration={1500}
            activeDot={{ r: 6, strokeWidth: 0, fill: "#D0BCFF" }}
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
};

export default BalanceChart;
