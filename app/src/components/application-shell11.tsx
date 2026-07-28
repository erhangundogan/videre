import {
  // Album,
  // BrushCleaning,
  // Calendar,
  ChevronDown,
  Home,
  LogOut,
  type LucideIcon,
  Menu,
  // Scan,
  ScanFace,
  Search,
} from "lucide-react";
import * as React from "react";
import { Link, useLocation } from "react-router-dom";
import { exit } from "@tauri-apps/plugin-process";

import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import { VidereLogo } from "@/components/videre-logo";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { Kbd } from "@/components/ui/kbd";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  useSidebar,
} from "@/components/ui/sidebar";
import { useIsMobile } from "@/hooks/use-mobile";
import { cn } from "@/lib/utils";

function getInitials(name: string) {
  return (
    name
      .split(" ")
      .filter(Boolean)
      .slice(0, 2)
      .map((part) => part[0]?.toUpperCase())
      .join("") || "U"
  );
}

const user = {
  name: "Erhan Gundogan",
  avatar: "",
};

type NavItem = {
  title: string;
  icon: LucideIcon;
  to?: string;
};

const navPrimary: NavItem[] = [
  { title: "Home", icon: Home, to: "/" },
  // { title: "Scan", icon: Scan },
  // { title: "Dedupe", icon: BrushCleaning },
  // { title: "Fix Dates", icon: Calendar },
  { title: "Face Detection", icon: ScanFace, to: "/" },
  // { title: "Smart Photos", icon: Album },
];

function NavPrimary({ items }: { items: NavItem[] }) {
  const location = useLocation();
  const activeIndex = items.findIndex((item) => item.to === location.pathname);
  return (
    <SidebarGroup>
      <SidebarMenu>
        {items.map((item, index) =>
          item.to ? (
            <SidebarMenuItem key={item.title}>
              <SidebarMenuButton asChild isActive={index === activeIndex} tooltip={item.title}>
                <Link to={item.to}>
                  <item.icon />
                  <span>{item.title}</span>
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ) : (
            <SidebarMenuItem key={item.title}>
              <SidebarMenuButton disabled aria-disabled tooltip={`${item.title} (coming soon)`}>
                <item.icon />
                <span>{item.title}</span>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ),
        )}
      </SidebarMenu>
    </SidebarGroup>
  );
}

function FooterLinks() {
  return (
    <div className="text-muted-foreground px-4 py-4 text-xs">
      <div className="mt-2 flex flex-wrap gap-x-2 gap-y-1">
        <a href="#" className="hover:underline">
          Terms
        </a>
        <a href="#" className="hover:underline">
          Privacy
        </a>
      </div>
    </div>
  );
}

function AccountMenuContent() {
  return (
    <DropdownMenuContent className="w-40" align="end">
      <DropdownMenuItem onClick={() => exit(0)}>
        <LogOut className="mr-2 size-4" />
        Quit
      </DropdownMenuItem>
    </DropdownMenuContent>
  );
}

function DesktopAccountMenu() {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" className="flex h-auto items-center gap-2 px-2 py-1">
          <Avatar className="size-8">
            <AvatarImage src={user.avatar} alt={user.name} />
            <AvatarFallback>{getInitials(user.name)}</AvatarFallback>
          </Avatar>
          <ChevronDown className="text-muted-foreground size-3" />
        </Button>
      </DropdownMenuTrigger>
      <AccountMenuContent />
    </DropdownMenu>
  );
}

function MobileAccountMenu() {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="size-9">
          <Avatar className="size-8">
            <AvatarImage src={user.avatar} alt={user.name} />
            <AvatarFallback>{getInitials(user.name)}</AvatarFallback>
          </Avatar>
        </Button>
      </DropdownMenuTrigger>
      <AccountMenuContent />
    </DropdownMenu>
  );
}

function SiteHeader() {
  const { toggleSidebar } = useSidebar();

  return (
    <header className="bg-background fixed top-0 z-50 hidden w-full items-center border-b md:flex">
      <div className="flex h-[var(--header-height)] w-[var(--sidebar-width-icon)] shrink-0 items-center justify-center">
        <Button className="size-9" variant="ghost" size="icon" onClick={toggleSidebar}>
          <Menu className="size-5" />
        </Button>
      </div>
      <div className="flex h-[var(--header-height)] items-center pr-4">
        <Link to="/" className="flex items-center gap-2">
          <div className="bg-primary flex size-8 items-center justify-center rounded-sm">
            <VidereLogo className="text-primary-foreground size-5" />
          </div>
          <span className="hidden text-lg font-semibold sm:block">Videre</span>
        </Link>
      </div>

      <div className="flex flex-1 justify-center px-4">
        <InputGroup className="h-10 max-w-xl rounded-full">
          <InputGroupAddon>
            <Search className="text-muted-foreground" />
          </InputGroupAddon>
          <InputGroupInput placeholder="Search" />
          <InputGroupAddon align="inline-end">
            <Kbd>⌘K</Kbd>
          </InputGroupAddon>
        </InputGroup>
      </div>

      <div className="flex h-[var(--header-height)] items-center gap-1 px-4">
        <DesktopAccountMenu />
      </div>
    </header>
  );
}

function AppSidebar({ ...props }: React.ComponentProps<typeof Sidebar>) {
  return (
    <Sidebar
      collapsible="icon"
      className="top-[var(--header-height)] hidden !h-[calc(100svh-var(--header-height))] md:flex"
      {...props}
    >
      <SidebarContent className="overflow-hidden">
        <ScrollArea className="min-h-0 flex-1">
          <NavPrimary items={navPrimary} />
        </ScrollArea>
      </SidebarContent>
      <SidebarFooter className="group-data-[collapsible=icon]:hidden">
        <FooterLinks />
      </SidebarFooter>
    </Sidebar>
  );
}

function MobileHeader() {
  return (
    <header className="bg-background sticky top-0 z-50 flex h-14 items-center justify-between border-b px-4 md:hidden">
      <Link to="/" className="flex items-center gap-2">
        <div className="bg-primary flex size-8 items-center justify-center rounded-sm">
          <VidereLogo className="text-primary-foreground size-5" />
        </div>
        <span className="text-lg font-semibold">Videre</span>
      </Link>
      <div className="flex items-center gap-1">
        <Button variant="ghost" size="icon" className="size-9">
          <Search className="size-5" />
          <span className="sr-only">Search</span>
        </Button>
        <MobileAccountMenu />
      </div>
    </header>
  );
}

function MobileBottomNav() {
  const location = useLocation();
  const activeIndex = navPrimary.findIndex((item) => item.to === location.pathname);
  return (
    <nav className="bg-background/95 fixed inset-x-0 bottom-0 z-40 border-t backdrop-blur md:hidden">
      <div
        className="grid"
        style={{ gridTemplateColumns: `repeat(${navPrimary.length}, minmax(0, 1fr))` }}
      >
        {navPrimary.map((item, index) => {
          const Icon = item.icon;
          const isActive = index === activeIndex;
          const className = cn(
            "flex flex-col items-center gap-1 py-2 text-xs transition-colors",
            isActive ? "text-foreground" : "text-muted-foreground hover:text-foreground",
            !item.to && "opacity-50",
          );
          return item.to ? (
            <Link key={item.title} to={item.to} className={className}>
              <Icon className="size-5" />
              <span>{item.title}</span>
            </Link>
          ) : (
            <button key={item.title} type="button" disabled aria-disabled className={className}>
              <Icon className="size-5" />
              <span>{item.title}</span>
            </button>
          );
        })}
      </div>
    </nav>
  );
}

export function ApplicationShell11({ children }: { children: React.ReactNode }) {
  const isMobile = useIsMobile();

  return (
    <div className="w-full [--header-height:3.5rem]">
      <SidebarProvider
        className="flex flex-col"
        style={{ "--sidebar-width-icon": "3rem" } as React.CSSProperties}
      >
        {isMobile ? (
          <div className="flex flex-col">
            <MobileHeader />
            <div className="pb-16">{children}</div>
            <MobileBottomNav />
          </div>
        ) : (
          <>
            <SiteHeader />
            <div className="flex flex-1 pt-[var(--header-height)]">
              <AppSidebar />
              <SidebarInset>{children}</SidebarInset>
            </div>
          </>
        )}
      </SidebarProvider>
    </div>
  );
}
