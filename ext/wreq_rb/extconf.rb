# frozen_string_literal: true

require "mkmf"
require "rb_sys/mkmf"

# Apply patches to vendored dependencies before compilation
patch_dir = File.expand_path("../../patches", __dir__)
wreq_dir = File.expand_path("../../vendor/wreq", __dir__)

if File.directory?(patch_dir)
  patches = Dir.glob(File.join(patch_dir, "*.patch")).sort
  # Patches stack in order and may touch the same files, so a patch only reverse-applies
  # cleanly while no later patch is applied. The newest patch that reverse-applies marks
  # itself and every earlier patch as already applied.
  applied_count = (patches.rindex do |patch|
    system("git", "apply", "--check", "--reverse", patch, chdir: wreq_dir, out: File::NULL, err: File::NULL)
  end || -1) + 1

  patches.each_with_index do |patch, index|
    if index < applied_count
      puts "Patch already applied: #{File.basename(patch)}"
    else
      puts "Applying patch: #{File.basename(patch)}"
      system("cd #{wreq_dir} && git apply #{patch}") || abort("Failed to apply #{patch}")
    end
  end
end

create_rust_makefile("wreq_rb/wreq_rb")
