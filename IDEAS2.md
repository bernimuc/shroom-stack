# Small ideas for the application

## Enable navigation in stack list preview

The application should allow navigating to the previous or next image in the preview mode
when the user presses left or right

## Mark image as include or exclude from stacking in preview

In preview mode it should be possible to include a file (by pressing p) or 
exclude it from the stack (by pressing x). This should only be possible for valid
input files, not for output files

## Allow the user to delete stacks or single images

Left to "Merge with previous" there should be a button that deletes all files from a stack.
It should be marked with a trash icon and have a red color indicating danger. Obviously there
should be a confirmation dialog that the user has to confirm before anything is deleted.

It also should be possible to delete single images, by selecting them and pressing DELETE 
or by choosing a delete action in the context menu.

Delete should work for both input and output files.

## Write processing parameters as metadata to output files

The method and the processing parameters used to generate an output file should be written 
to the output files.

## Display stored processing parameters

For output files the processing parameters should be shown somewhere. The method could
be shown as a small icon overlayed on the thumbnail in the top left corner. Detail parameters
could be shown in the tooltip.

## allow multiple selection in stack list

It would be nice if the user can select multiple images in the stack list. This would 
be especially useful for deleting and moving images between stacks.

## Don't overwrite existing output files

When saving the output files, the automatically created name should be unique by appending
a number at the end of the filename, thus avoiding overwriting existing files. Overwriting 
only should happen when the user specifies an existing file when saving and confirms that
it should be overwritten.

## Start the app in Session workspace and rename that workspace

I found that the session workspace is very useful to start in. Lets make that the starting
view of the app.

Rename that workspace to "Manage stacks"

## Remove Processing workspace

The "processing" workspace does not have any unique functionality and the user can 
not really interact with it. Lets remove it, its functionality (displaying the 
status bar and the maps as they are created) can be covered by workspace "Postprocessing"