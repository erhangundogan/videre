export function VidereLogo({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 405 400" fill="none" xmlns="http://www.w3.org/2000/svg" className={className}>
      <g stroke="currentColor">
        <g fill="currentColor">
          <circle cx="16" cy="69" r="15.5" />
          <circle cx="205" cy="16" r="15.5" />
          <circle cx="389" cy="69" r="15.5" />
          <circle cx="205" cy="376" r="23.5" />
        </g>
        <path d="m36.2942 102.294 136.7198 236.806" strokeLinecap="round" strokeWidth="18" />
        <path d="m204.07 330.983v-273.4397" strokeLinecap="round" strokeWidth="18" />
        <path d="m233.706 339.1 136.72-236.806" strokeLinecap="round" strokeWidth="18" />
      </g>
    </svg>
  );
}
