"use client";

import { Menu, Users } from "lucide-react";
import * as React from "react";
import { Link } from "react-router-dom";

import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { cn } from "@/lib/utils";

// Nav item - links to a route
type NavItem = {
  label: string;
  icon: React.ComponentType<React.SVGProps<SVGSVGElement>>;
  to: string;
};

// Sidebar data - videre's single "Faces" nav item
const sidebarData: {
  title: string;
  navItems: NavItem[];
} = {
  title: "videre",
  navItems: [{ label: "Faces", icon: Users, to: "/" }],
};

// Mobile navigation sheet
const MobileNav = () => {
  return (
    <Sheet>
      <SheetTrigger asChild>
        <Button variant="ghost" size="icon" className="md:hidden">
          <Menu className="size-5" />
        </Button>
      </SheetTrigger>
      <SheetContent side="left" className="w-72 p-0">
        <SheetHeader className="px-4 pt-4">
          <SheetTitle className="flex items-center gap-2">
            {sidebarData.title}
          </SheetTitle>
        </SheetHeader>
        <ScrollArea className="min-h-0 flex-1">
          <nav className="flex flex-col gap-1 px-4 py-4">
            {sidebarData.navItems.map((item) => {
              const Icon = item.icon;
              return (
                <Link
                  key={item.label}
                  to={item.to}
                  className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-muted"
                >
                  <Icon className="size-4" />
                  {item.label}
                </Link>
              );
            })}
          </nav>
        </ScrollArea>
      </SheetContent>
    </Sheet>
  );
};

interface ApplicationShell4Props {
  className?: string;
  children?: React.ReactNode;
}

export function ApplicationShell4({
  className,
  children,
}: ApplicationShell4Props) {
  return (
    <div className={cn("flex min-h-svh flex-col", className)}>
      {/* Top navigation bar */}
      <header className="sticky top-0 z-50 bg-background">
        <div className="flex h-14 items-center gap-4 border-b px-4 lg:px-6">
          {/* Mobile menu */}
          <MobileNav />

          {/* Logo */}
          <Link to="/" className="flex items-center gap-2">
            <div className="flex aspect-square size-8 items-center justify-center rounded-sm bg-primary">
              <Users className="size-5 text-primary-foreground" />
            </div>
            <span className="font-semibold">{sidebarData.title}</span>
          </Link>

          {/* Desktop navigation */}
          <nav className="ml-4 hidden items-center gap-1 md:flex">
            {sidebarData.navItems.map((item) => (
              <Button key={item.label} variant="ghost" className="gap-1" asChild>
                <Link to={item.to}>{item.label}</Link>
              </Button>
            ))}
          </nav>
        </div>
      </header>

      {/* Main content */}
      <main className="flex flex-1 flex-col gap-4 p-4 lg:p-6">{children}</main>
    </div>
  );
}
